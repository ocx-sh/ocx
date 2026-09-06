# Spec review — `plan_index_claim_command.md`

**Reviewer:** reviewer (focus: spec), Opus 5
**Target:** `.claude/artifacts/plan_index_claim_command.md`
**Sources of truth:** `adr_index_claim_command.md` (Accepted), `system_design_index_claim_command.md`,
`meta-ai-config.md` § Plan Status Protocol, `plan.template.md`, `CLAUDE.md`,
`~/.claude/skills/hex-core/references/protocol.md`
**Date:** 2026-09-05

---

## Mechanical check results

| # | Check | Result |
|---|---|---|
| 1 | **Traceability — Scope cells** | 66 `C-001…C-066` + 37 `S-001…S-037` defined, contiguous, no duplicates. **103/103 appear in at least one work-package `Scope` cell.** Pass. |
| 1b | **Traceability — named tests** | **9 of 103 have no named test** in the Test inventory: `C-003`, `C-014`, `C-018`, `C-032`, `C-054`, `C-066`, `S-005`, `S-015`, `S-035`. See table below. |
| 2 | **Reverse coverage** | Every ID cited in a `Scope` cell exists. **0 dangling citations.** Pass. |
| 3 | **ADR Implementation-Plan step coverage** | **10/10 steps have an owning work package.** 0 Blocks. (Step 1 → WP-1/WP-5/WP-7/WP-15; 2 → WP-5/6/7; 3 → WP-5/7/11; 4 → WP-2/WP-9; 5 → WP-10/11/12; 6 → WP-10; 7 → WP-8; 8 → WP-6/7/11/12; 9 → WP-4/13/14; 10 → WP-15/WP-16.) |
| 4 | **ADR Validation coverage** | 27 checklist items. **4 have no home** — "every new exit-code mapping" (F-07); the two `AuthError`/80 credential arms (F-16); "the website build" (F-22); "the claim branch" in item 1 (F-13). 3 more are covered only on one side: owner-provenance rule 1's canonical-login override, the pre-existing-token negative half (F-24), the `git-version` row `detail` on GitLab. |
| 5 | **Handoff-decisions coverage** | **7/7 rows land.** 0 Blocks. (1 → Overview + "Shippable after wave 5"; 2 → WP-16 gate 1; 3 → WP-16 closeout row 6; 4 → C-062/DV-3/WP-12; 5 → WP-16 gate 5; 6 → WP-16 closeout row 4; 7 → WP-16 closeout row 5.) |
| 6 | **File-set disjointness** | **FAIL — 3 collisions** after path normalization (WP-5 writes `forge/github.rs`, `forge/gitlab.rs`, `forge/git_workspace.rs` in shorthand; WP-6/7/8 write the same three files fully qualified). Literal string comparison of the 16 cells finds 0 — the shorthand is what hides it. See F-04. |
| 7 | **DAG soundness** | Pass. All 13 declared edges are wave-monotone (dependency wave < dependent wave in every case). The mermaid graph reproduces the `Depends on` column exactly — no edge in one and not the other. Critical path recomputed: `WP-1 → WP-5 → WP-9 → WP-11 → WP-12 → WP-14 → WP-16`, 7 nodes, is the longest chain (next longest, `WP-2 → WP-9 → …`, is 6; `WP-1 → WP-5 → WP-8 → WP-14 → WP-16` is 5). "four of them large" is correct (WP-5, 9, 11, 14). |
| 8 | **Review / Verify budgets** | 16 rows checked against `protocol.md` § 514 (`self` = docs-only/tiny, no security-sensitive files; `panel` = large, cross-area, security- or hot-path-touching). **1 under-budget**: WP-6 `light` on the file that decides `ForgeIdentity::bot` (F-12). **1 sub-overhead WP with no justification**: WP-3 (F-26). WP-1's `light` on a one-way wire vocabulary is borderline (F-27). `Verify: full` on WP-5/14/16 is justified in prose. |

### IDs failing the named-test half of check 1

| ID | What it is | Nearest named test | Verdict |
|---|---|---|---|
| `C-003` | `PermissionDenied` doc widened | — | doc-only, no marker (F-14) |
| `C-014` | `WriteTransport` wire spellings + `Default = Api` | `validate_transport_refuses_github_git` (behaviour, not spelling/default) | uncovered (F-14) |
| `C-018` | every `ForgeError` variant's `ClassifyExitCode` mapping | `claim_error_classify_delegates_through_forge` (delegation only) | uncovered (F-07) |
| `C-032` | `forge/gitlab.rs` doc comment rewritten whole | — | doc-only, no marker (F-14) |
| `C-054` | claim branch name `indexbot-claim-<ns>-<pkg>` | `test_rerun_updates_same_request` (does not assert the name) | uncovered (F-13) |
| `C-066` | three-edit checklist; `OCX_ANNOUNCE_GIT_USERNAME` must **not** enter `CREDENTIAL_KEYS` | — | uncovered (F-10) |
| `S-005` | announce's `UnclaimedNamespace` → 79 preserved | — (an existing repo test pins it, uncited) | uncovered (F-15) |
| `S-015` | job-token push disabled on the target project → 86 | `test_two_signal_old_instance_86` (the *unknown* path, not this one) | uncovered (F-01) |
| `S-035` | `ocx index claim` did-you-mean + `ocx index` group help | — | uncovered (F-13) |

---

## Findings

### F-01 · Block · The preflight's readable-`false` abort is missing, so a job-token-push-disabled project exits 77 instead of 86

**Where:** Component contracts § GitLab REST, `C-029`; § Git workspace, `C-044`; UX scenario `S-015`

**Claim:** C-029 — "the preflight reads `GET /projects/:id` for `permissions.project_access.access_level >= 30` and `ci_push_repository_for_job_token_allowed` … An unreadable field reports `unknown` and the run proceeds."
C-044 — "and — **only when the preflight reported `job-token-push: unknown`** — an HTTP 403 or a `not allowed to push` line → `WriteCapabilityUnavailable` (86). The same text with the preflight reporting `passed` lands on 77."
S-015 — "`--transport git` where job-token push is disabled on the target project | **86**, message naming Settings → CI/CD → Job token permissions and the project path".

**Problem:** C-029 describes only two preflight outcomes — read (proceed) and unreadable (`unknown`, proceed). The ADR's preflight table gives three, and the third is the primary 86 trigger:

> `job-token-push` … `GET /projects/:id` → `ci_push_repository_for_job_token_allowed` | `false` → `ForgeError::WriteCapabilityUnavailable`, exit 86, message naming Settings → CI/CD → Job token permissions and the project path; field unreadable → `unknown`, run proceeds
> — ADR § API Contract — the preflight

The same row exists for `job-token-allowlist` ("publishing project absent → `WriteCapabilityUnavailable`, exit 86, naming both project paths"). Under the plan as written a readable `false` has no representation: `C-012` states "`CheckStatus` renders `passed`, `unknown`, `skipped`. There is **no `Failed`**", so the check cannot report the refusal as a row, and C-029 does not say the call returns `Err`. The run therefore proceeds to the push, is refused, and C-044 — whose promotion is gated on `unknown` — lands it on **77**. That is a wrong published exit code on the exact condition [ocx#411](https://github.com/ocx-sh/ocx/issues/411) reports, and `S-015` (the scenario that names the correct code) is one of the nine IDs with no named test, so nothing would catch it.

**Fix:** Extend `C-029` with the third outcome, explicitly as a `ForgeError` rather than a `CheckStatus`: "a readable `ci_push_repository_for_job_token_allowed: false` returns `ForgeError::WriteCapabilityUnavailable` (86) from `ensure_push_access` **before any push**, with the Settings → CI/CD → Job token permissions message and the project path; an allowlist read that does not contain the publishing project does the same, naming both paths. Only an *unreadable* field yields `unknown`-and-proceed." Add `preflight_readable_false_errs_86` to WP-7's unit row and `::test_job_token_push_disabled_86_before_any_push` to WP-14's acceptance row, and state in `C-044` that the classifier's promotion covers only the `unknown` path because the readable-`false` path never reaches it.

---

### F-02 · Block · The recording `git` shim has no work package, no file and no test

**Where:** Parallelization § Work packages, WP-4 and WP-14; Test inventory, WP-4 row

**Claim:** WP-4 — "git-over-HTTP test fixture. Serves C-030, C-033…C-045, S-013…S-033 | `test/tests/git_http_fixture.py` (new), `test/tests/fake_forge.py`", with five named tests, none of which mentions a shim. WP-14's Expected files are `test/tests/test_transport_git.py` (new), `test/tests/announce_helpers.py`.

**Problem:** The string `shim` occurs exactly once in the plan, in the unrelated `cargo build … -p ocx_shim` line. The ADR makes the shim a mandatory fixture component and says so in the strongest terms:

> A **recording `git` shim placed earlier on the child's `PATH`**, capturing argv and the environment block to a file the test reads before delegating to the real binary. **Without it no assertion about credential containment is possible at all**: the existing fake is an in-process `http.server` with no subprocess visibility.
> — ADR § Test fixture

Two entire Validation bullets are keyed to it — "**Credential-leak assertions**, via the recording `git` shim" and "**Child-environment assertions** (same shim)" — and six of WP-14's named tests cannot be written without it: `::test_secret_absent_from_argv_config_url_and_stderr`, `::test_each_secret_form_proved_red_then_green`, `::test_ambient_git_trace_does_not_reach_child`, `::test_ambient_http_proxy_passed_through`, `::test_no_helper_invoked_on_injecting_run`, `::test_helper_invoked_on_step_three_run`. An executor reading WP-4's Expected files builds the CGI bridge and stops; WP-14 then either invents the shim inside a file it does not own, or the six assertions quietly become weaker HTTP-side checks — which is the silent-downgrade failure mode `quality-core.md` § Unchecked Green exists to prevent.

**Fix:** Add the shim to WP-4 as a named deliverable with its own file (e.g. `test/tests/git_recording_shim.py` plus the generated executable it writes onto the child `PATH`), extend WP-4's Scope to cite `C-034`, `C-035`, `S-030…S-033`, and add a WP-4 self-test that proves the shim captures argv **and** the environment block and then delegates — `::test_shim_records_argv_and_env_then_delegates`. Re-price WP-4's `L` estimate to include it; the researched ~150–220 lines covers the CGI bridge only.

---

### F-03 · Block · The request title/body fixed-template invariant is absent from the contracts and the test inventory

**Where:** Component contracts § Git workspace, `C-039`; § Claim library (no contract); Test inventory, WP-8

**Claim:** C-039 — "the push carries **exactly four** options — `merge_request.create`, `.target`, `.title`, `.description` — and no fifth … Every rendered option value is rejected if it contains a newline, a NUL or any other control character, before it reaches the pkt-line." WP-8's only related test is `control_character_in_option_value_is_refused`.

**Problem:** The plan keeps the derived guard and drops the invariant it derives from. The ADR states the template rule as a security invariant:

> The claim and announce request title and body are built from a fixed template carrying only structured values — the logical name, the physical repository, the branch, and the resolved `login:id` pairs. **No operator free text is interpolated.** `--upstream-disclaimer` reaches the **root file only**, where the serializer escapes it, and never the request title or body.
> Owner logins are rendered as plain `login:id` text, deliberately **without** an `@`, so the body fires no mentions.
> — ADR § Security Architecture 3

and explicitly makes the control-character check cheap *because* of it: "Under the fixed-template rule below no such character can occur, which is precisely what makes the check cheap and its failure a loud signal that something upstream changed." With the premise unstated, the plan ships a guard whose cost model assumes a rule nobody wrote down — a guard's premise falsified from another file. Concretely, nothing in the plan forbids an implementer from interpolating `--upstream-disclaimer` (an operator free-text flag, `C-057`) into `merge_request.description`, and no named test would red. The `@`-suppression rule, which is what stops a claim from mass-mentioning accounts in a repository humans review under G-04, is likewise nowhere.

The gap is not git-only: the REST path opens the same request through `open_or_update_pull_request`, and the plan has no body contract there either. The ADR's own deviation table records that `.description` was **added as a fourth push option** for this content ("the reviewer needs the rendered `login:id` in the request body"), so the body is a decided part of the design, not an implementation detail.

**Fix:** Add a contract in § Claim library (and cite it from `C-039`): "**C-0xx** — the request title and body are one fixed template over structured values only — logical name, physical repository, branch, and the resolved `login:id` pairs rendered without a leading `@` — plus the `owner_identity_source` word. No flag-supplied free text is interpolated; `--upstream-disclaimer` and `--upstream-org` reach the root file only." Add `request_body_is_fixed_template_no_free_text` and `owner_logins_render_without_at_sign` to WP-9's unit row, and `::test_disclaimer_absent_from_request_title_and_body` to WP-13's acceptance row (the REST path, where it is observable).

---

### F-04 · Block · Three files appear in two work packages' Expected files, contradicting the plan's own ownership claim

**Where:** Parallelization § preamble; Work packages table, WP-5 vs WP-6 / WP-7 / WP-8; Merge plan

**Claim:** "**File sets are disjoint by construction and by ownership**: every file below is owned by exactly one work package for the life of the plan." And: "Each merge re-validates its file set with `git diff --name-only <base>..<wp-branch>` against the Expected Files column before merging."

**Problem:** After path normalization the sets intersect three times:

| Pair | File |
|---|---|
| WP-5 ∩ WP-6 | `crates/ocx_lib/src/forge/github.rs` |
| WP-5 ∩ WP-7 | `crates/ocx_lib/src/forge/gitlab.rs` |
| WP-5 ∩ WP-8 | `crates/ocx_lib/src/forge/git_workspace.rs` |

WP-5 spells them in shorthand (`forge/github.rs`, `forge/gitlab.rs`, `forge/git_workspace.rs`), which is why a literal comparison of the 16 cells reports zero collisions; WP-6/7/8 spell the same paths fully. The plan knows about the overlap in prose — "WP-5 lands the whole contract plus stub bodies, and the later packages fill disjoint files behind it" — but the ownership sentence and the merge-time predicate both read as absolutes and are false as written. The concrete cost is the merge gate: WP-6's `git diff --name-only` produces `crates/ocx_lib/src/forge/github.rs`, a path WP-5 already claimed, so the check either fires a false positive on every wave-3 merge or is silently ignored, which makes it exactly the unchecked green the plan's own § Red/green discipline warns about. (There is no *concurrency* hazard — WP-6/7/8 all depend on WP-5 and sit a wave later — so this is a stated-invariant and gate-integrity defect, not a collision risk.)

**Fix:** Spell every path fully in the WP-5 cell, then replace the ownership sentence with the truth the plan already argues: "File sets are disjoint **within a wave**. Three files are written twice across waves — `forge/github.rs`, `forge/gitlab.rs`, `forge/git_workspace.rs` — by WP-5 (contract + stub bodies) and then by WP-6/WP-7/WP-8 (implementation); each pair is serialized by a declared dependency edge." Amend the merge predicate to allow a re-write of a file whose earlier owner is a declared ancestor, and reject any other overlap.

---

### F-05 · High · DV-4 names the wrong drift gate; the file that actually holds the six-page list is owned by nobody

**Where:** Deviations from the ADR, `DV-4`; Work packages, WP-15 Expected files; Test inventory, WP-15 row; Documentation surfaces table

**Claim:** DV-4 — "A one-tree consistency gate (`test/tests/test_doc_scripts_one_tree.py`) covers only the six pages that use `<<<` transclusion … The new page joins the transcluded set so it cannot drift." WP-15's Expected files list `test/tests/test_doc_scripts_one_tree.py`; the Test inventory says "`test_doc_scripts_one_tree.py` gains the new page".

**Problem:** `test/tests/test_doc_scripts_one_tree.py` is not that gate. Its module docstring says it guards "the one-tree convergence (ADR H-4a …): **EQ1** — the legacy `test/recordings/scripts/` tree stays gone; **EQ2** — no slug is backed by >1 file …; **EQ3** — no second discovery path …; **EQ3b** — the cast-orphan sweep is manifest-scoped". Every one of its tests is glob-driven; it carries no page list and would need **no edit** for a new page.

The six-page set lives in a different file, in the symbol `WALKTHROUGH_PAGES` in `test/src/doc_binding.py`:

> `WALKTHROUGH_PAGES: tuple[Path, ...] = (getting-started.md, user-guide.md, faq.md, in-depth/environments.md, in-depth/entry-points.md, in-depth/lazy-loading.md)`
> `"""The six walkthrough pages subject to NC1–NC3 checks. … These are the prose pages whose inline code blocks must all be `<<<` transclusions backed by tested doc scripts."""`

So DV-4's anti-drift guarantee — the entire justification for the deviation — would not land: WP-15 would edit a glob-driven structural test (a no-op), leave `WALKTHROUGH_PAGES` at six, and ship the new page outside the NC1–NC3 checks, i.e. in the "hand-typed and **provably drifted today**" set DV-4 exists to escape. Worse, `test/src/doc_binding.py` is in no work package's Expected files, so the merge-time file-set check would *reject* the edit that fixes it.

**Fix:** In DV-4 and the Documentation surfaces table, replace `test/tests/test_doc_scripts_one_tree.py` with `test/src/doc_binding.py` (`WALKTHROUGH_PAGES`); add that path to WP-15's Expected files and drop the one-tree file unless a concrete EQ-level edit is identified. Change the WP-15 test row to "`WALKTHROUGH_PAGES` gains `user-guide/claiming-a-namespace.md`; `test_doc_binding.py`'s NC1–NC3 run against it", and prove it red by adding an untranscluded `ocx` fence to the new page before removing it.

---

### F-06 · High · `http.followRedirects=false` is scoped to clone hygiene; the ADR requires it on every invocation

**Where:** Component contracts § Git workspace, `C-033`

**Claim:** "**C-033** — **clone hygiene**, all mandatory: tempdir mode `0700` **on Unix only**, `-c core.symlinks=false`, never `--recurse-submodules`, `http.followRedirects=false`, `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`, `LC_ALL=C`, `LANGUAGE=`."

**Problem:** The ADR scopes redirect suppression to every git call, not to the clone:

> **Every invocation below carries `-c http.followRedirects=false`**; every invocation **that injects an ocx credential** additionally carries `-c credential.helper=` and the `GIT_CONFIG_*` credential pair.
> — ADR § The git recipe

The plan gets the credential-helper half right — `C-034` says "`-c credential.helper=` is carried on **every invocation that injects an ocx credential** and only those" — and then buries the redirect flag inside a list headed "clone hygiene". The invocation that matters most is step 5, the **push**, which is precisely where the `http.<prefix>.extraHeader` credential is attached: a redirect followed there sends `Authorization: Basic <base64(user:secret)>` to a host ocx did not choose. Reading C-033 literally produces `git … fetch -c http.followRedirects=false …` and a bare `git … push`, and no named test in WP-8 or WP-14 asserts the flag on the push.

**Fix:** Move the flag out of C-033 into its own sentence with the ADR's scope — "`-c http.followRedirects=false` is carried on **every** git invocation, not only the clone" — or restate C-033 as "workspace hygiene" and add the per-invocation rule beside C-034. Add `follow_redirects_disabled_on_every_invocation` to WP-8's unit row and assert it from the recording shim's argv capture in WP-14.

---

### F-07 · High · No named test covers the `ForgeError` → exit-code mapping (`C-018`), and the ADR Validation item that demands one has no home

**Where:** Component contracts § Forge public surface, `C-018`; Test inventory, WP-5 and WP-8 rows

**Claim:** "**C-018** — `ForgeError` gains `TransportUnsupported`, `TransportOperationUnsupported`, `UsersApiUnavailable`, `GitUnavailable`, `GitCommandFailed`, `GitPushFailed`, `StaleLease`, `PushRefused`, `WriteCapabilityUnavailable`, `MergeRequestUnconfirmed`, each mapped by `ClassifyExitCode` per the ADR exit-code table. `GitCommandFailed` is deliberately unclassified (exit 1)."

**Problem:** Ten new variants carrying seven distinct published exit codes (64, 69, 75, 77, 86, and two deliberate exit-1 fall-throughs), and the Test inventory names no test over them. WP-5's row has nothing; WP-9's `claim_error_classify_delegates_through_forge` proves only that `ClaimError` forwards to the inner error, not what the inner error maps to. The ADR's Validation checklist asks for exactly this and gets no owner:

> **Unit (Rust)**: root rendering …; owner serialisation; `ForgeKind::validate_transport` refusals; **every new exit-code mapping**; the git stderr classifier over each phrase.
> — ADR § Validation

Exit codes are a published contract (`CLAUDE.md`: "Interfaces are the CLI surface and every wire/persisted format … exit codes"), and the ADR lists 86 among the five genuinely one-way items. A `_ => None` fall-through added by accident to `WriteCapabilityUnavailable` degrades it to exit 1 and compiles clean — the exact defect the `error_category.rs` module doc records for the old cross-crate form.

**Fix:** Add `forge_error_exit_code_table` to WP-5's unit row — one table-driven test enumerating all ten variants against the ADR's exit-code table, with the two deliberate exit-1 cases (`GitCommandFailed`, `GitPushFailed`) asserted as `None` rather than omitted, so a later accidental classification also reds. Prove it red by mutating one arm.

---

### F-08 · High · The `credential_kind` / `push_credential_kind` value vocabularies are missing from `C-060` and `C-061`

**Where:** Component contracts § CLI, `C-060`, `C-061`

**Claim:** "**C-060** — `ClaimReport` carries `package`, `name`, `status`, `forge`, `transport`, `credential_kind`, `push_credential_kind`, `author`, `owners`, `owner_identity_source`, `branch`, `pull_request_url`, `pull_request_number`, `fork`, `written_paths`, `capability_checks`." "**C-061** — the announce report gains `forge`, `transport`, `credential_kind`, `push_credential_kind`, `branch`, `capability_checks`."

**Problem:** The key names are contracted; the **values** are not. The ADR fixes them:

> `credential_kind` | `"job-token"` \| `"token"` \| `"none"` … reporting a kind ocx cannot observe would be a lie a pipeline could assert on.
> `push_credential_kind` | `"job-token"` \| `"token"` \| `"git-helper"` \| `null` | The push credential; `null` under `api`.
> `status` | `"unchanged"` \| `"updated"` | … Claim compares against the **open claim branch** … and its `--out` is therefore always `updated`.
> — ADR § API Contract — the `--format json` report

These are one-way by the ADR's own reckoning ("The new JSON keys on announce's already-shipped report … Additive today, unremovable tomorrow"), and the plan contracts the equivalent vocabularies everywhere else — `C-012` pins `CapabilityName`/`CheckStatus` spellings, `C-046` pins `owner_identity_source`'s three words. These three are the omission. `S-029` mentions `"git-helper"` in passing and `S-002` mentions `"resolved"`, but a tester writing `announce_report_gains_six_keys` from C-061 has no way to know that `credential_kind` may not report `pat` or `deploy-token`, which is the specific error the ADR narrows the operability lane's proposal to prevent.

**Fix:** Extend `C-060` with the three value sets and the claim-specific `status` referent ("claim compares against the open claim branch, so `--out` always reports `updated`"), and have `C-061` inherit them by reference. Add `credential_kind_wire_spellings` to WP-11's unit row and assert the same set from `announce_report_gains_six_keys` in WP-12.

---

### F-09 · High · WP-16's Scope says three cross-repo issues; its own closeout table lists five plus a conditional sixth

**Where:** Work packages, WP-16 row; § Cross-repository closeout (WP-16)

**Claim:** WP-16 — "Release gates: blobless-clone measurement, partial-clone proof, **three cross-repo issues**, the two issue-body posts". Two sections later: "**Five issues and two posts**, none of which can land in this repository", over a table with six issue rows (corporate-CA REST client, ocx-mirror transport fields, ocx-catalog owner href, indexbot dual-emit stop date, `actor_id` governance, reviewer checklist) plus the issue-body-posts row.

**Problem:** The two counts disagree by a factor of two in the plan's own terminal work package. "Three" is the stale figure — it matches ADR step 10 ("**three follow-up issues filed**") before the Handoff decisions added three more (indexbot dual-emit, `actor_id`, reviewer checklist), each of which the ADR's own table routes to an index issue. An executor working from the Parallelization table — which the plan declares canonical ("The table is canonical; the graph is its index") — files three and drops three, and the drop is invisible because WP-16 has no test. Two of the dropped three are Handoff rows this review verified as landing *only* through that closeout table, so the count error silently un-lands them. The repository's own history makes this expensive to recover: a PR footer "Closes #a, #b, #c" closes only `#a`, so each number has to be audited individually after the merge.

**Fix:** Change the WP-16 Scope cell to "five cross-repo issues (six if the index reviewer checklist is absent), the two issue-body posts", and add the conditional row's decision procedure to the closeout table so "verify … if absent, open an issue" has a recorded outcome. Add a WP-16 completion checklist enumerating the six items by name so the gate can be audited.

---

### F-10 · High · `C-066`'s three-edit checklist is split across two work packages two waves apart, against its own "in one change"

**Where:** Component contracts § CLI, `C-066`; Work packages, WP-11 (wave 4) and WP-15 (wave 6)

**Claim:** "**C-066** — `OCX_ANNOUNCE_GIT_TOKEN` completes the three-edit checklist **in one change**: `.claude/rules/subsystem-cli.md`'s exemption table, `ocx_lib::env::keys::CREDENTIAL_KEYS`, and `website/src/docs/reference/environment.md`. `OCX_ANNOUNCE_GIT_USERNAME` is **not** a credential and must **not** enter `CREDENTIAL_KEYS`."

**Problem:** WP-11's Scope takes "C-066 (the `CREDENTIAL_KEYS` edit)" and owns `crates/ocx_lib/src/env.rs`; WP-15 owns `.claude/rules/subsystem-cli.md` and `website/src/docs/reference/environment.md` and lands two waves later. So the contract that says "in one change" is executed as three changes across two packages, and between wave 4 and wave 6 the tree carries a scrubbed credential variable that neither the exemption table nor the reference documentation mentions. That is the exact drift the checklist exists to prevent — `subsystem-cli.md`'s own table records that `ocx_lib::env::keys::CREDENTIAL_KEYS` "carries the same two notes", i.e. the two artifacts are a matched pair by design. The ADR keeps them together too: "**Three-edit checklist for `OCX_ANNOUNCE_GIT_TOKEN` in this step**" (Implementation Plan step 3). Neither half is covered by a named test, and the negative assertion — `OCX_ANNOUNCE_GIT_USERNAME` must **not** be in `CREDENTIAL_KEYS` — is the kind of rule that only ever fails silently.

**Fix:** Move all three edits into WP-11 (add the two documentation paths to its Expected files and remove them from WP-15's), or record the split as a seventh row in the Deviations table with the interim state named. Either way add `credential_keys_contains_git_token_not_username` to WP-11's unit row, asserting both the positive and the negative membership, and note that `crates/ocx_cli/src/app/plugin_dispatch.rs` consumes the list generically and needs no edit.

---

### F-11 · High · The `--package` → positional window leaves 89 existing invocations in six acceptance modules no work package owns

**Where:** Component contracts § CLI, `C-062`; Work packages, WP-12 and WP-14; Scope § In scope

**Claim:** "**C-062** — `announce`'s `--package` becomes a **hidden** argument; the positional is canonical; exactly one of the two is required … All of it lives in `deprecated.rs`." WP-12's Expected files are `package_announce.rs`, `api/data/announce.rs`, `deprecated.rs`; WP-14 owns `test/tests/announce_helpers.py`.

**Problem:** `--package` is invoked 89 times across six acceptance modules that appear in no work package's Expected files: `test_announce.py` (48), `test_announce_gitlab.py` (33), `test_tag_reserved.py` (4), `test_exit_codes.py` (2), `test_package_cascade.py` (1), `test_announce_push_file.py` (1). Three consequences:

1. WP-12's `Verify: scoped` runs `task rust:verify` only, so nothing in wave 5 executes those 89 invocations against the new grammar. (They will not red on stderr — I checked; no announce test asserts an empty stderr — but that is luck, not design.)
2. After the window opens, the **canonical** positional form has essentially no acceptance coverage while the deprecated form has 89 call sites. The batched-window carve-out in `CLAUDE.md` names 0.7 as the removal release; at 0.7 those 89 sites red at once, in a repository that will by then have forgotten why.
3. The merge-time file-set check would reject the migrating edit, because none of the six modules is in any Expected-files cell.

**Fix:** Add the six modules to WP-12's Expected files (or to a new sibling package in wave 5) and make the migration part of `C-062`: "every in-repository invocation moves to the positional form in the same change; the hidden `--package` form retains exactly one dedicated test." Add `::test_announce_positional_form_is_the_default_in_the_suite` — a structural test asserting no `--package` remains outside the deprecation test — so the 0.7 removal is a one-file deletion as the plan promises.

---

### F-12 · High · WP-6 carries a `light` review budget on the file that decides `ForgeIdentity::bot`

**Where:** Work packages, WP-6 row

**Claim:** "**WP-6** | GitHub REST identity + preflight. C-023…C-025 | `crates/ocx_lib/src/forge/github.rs` | M | 3 | WP-5 | **light** | scoped".

**Problem:** `protocol.md` assigns budgets by content, not by size: "docs-only or tiny low-risk work (~≤50 expected lines, no security-sensitive or hot-path files) → `self`; a single-area moderate change → `light`; large, cross-area, **security- or hot-path-touching** work → `panel`." WP-6 implements `C-023` — "`authenticated_identity` reads `GET /user`; `type == "Bot"` sets `bot: true`" — which is the strong half of the impersonation control the ADR builds `owners[]` provenance on:

> **The bot check is only as strong as the source allows.** … On the unreachable path it degrades to a name-shape heuristic … That residual is why rule 2 exists: the reviewer, not the heuristic, is the control.
> — ADR D-C4 rule 3

and whose STRIDE Spoofing row leans on the index reviewer verifying `owners[]` against forge profiles. A wrong `bot` flag grants G-19 owner-gated auto-merge to a machine account. Its sibling WP-7, doing the same job on GitLab, correctly carries `panel`; the asymmetry has no stated reason.

**Fix:** Raise WP-6 to `panel`. If the budget is being conserved, the defensible alternative is `light` **plus** a named security perspective in its review scope — but the file that decides `bot` should not be reviewed more cheaply than the file that decides it on the other forge.

---

### F-13 · High · Two ratified decisions ship untested: the claim branch name (`C-054`) and the `ocx index claim` mitigation (`S-035`)

**Where:** Component contracts § Claim library, `C-054`; UX scenarios, `S-035`; Test inventory, WP-9, WP-11, WP-13

**Claim:** "**C-054** — the claim branch is `indexbot-claim-<namespace>-<package>`, distinct from `indexbot-announce-<namespace>-<package>`." "**S-035** | `ocx index claim` | clap did-you-mean hint points at `ocx package claim`; the `ocx index` group's help states that **no index subcommand writes to a forge**."

**Problem:** Neither has a named test.

`C-054` is D-C6, and the branch name is load-bearing on the far side of a repository boundary: "so indexbot's G-04 `new-package` classification is never confused with a refresh, and a claim and a later announce never share a branch." The ADR's first Validation bullet asks for "**the claim branch**" among the REST assertions; WP-13's `::test_rerun_updates_same_request` proves idempotence without ever reading the name. A one-character drift produces a claim indexbot silently classifies as a refresh.

`S-035` is the entire mitigation for the chosen option's only real con — D-C1: "Mitigation for the discoverability cost: `ocx index claim` gets a clap did-you-mean hint, and the `ocx index` group's help states that **no index subcommand writes to a forge**." The ADR then insists the sentence is contract text: "Help text is contract text under `quality-cli-help.md`, so shipping the old sentence would ship a documented lie a user could act on." WP-11's six unit tests and WP-13's sixteen acceptance tests contain nothing for either half, so a mitigation the option matrix was decided on could be forgotten with no signal.

**Fix:** Add `claim_branch_name_is_distinct_from_announce` to WP-9's unit row (asserting both spellings from one namespace/package pair) and `::test_claim_branch_name` to WP-13's acceptance list. Add `index_claim_suggests_package_claim` and `index_group_help_states_no_forge_write` to WP-11's unit row.

---

### F-14 · Warn · `C-014`, `C-003` and `C-032` have no named test and no "reviewed, not tested" marker

**Where:** Component contracts `C-003`, `C-014`, `C-032`; Test inventory, WP-1, WP-5, WP-7 rows

**Claim:** "**C-014** — `WriteTransport { Api, Git }`, a clap `ValueEnum` spelling `api` / `git`, `Default = Api`." "**C-003** — `ExitCode::PermissionDenied`'s doc comment covers a forge-side branch-protection refusal … Doc-only; no mapping changes." "**C-032** — the `forge/gitlab.rs` doc comment is rewritten **whole**, correcting both wrong sentences."

**Problem:** The plan opens the Test inventory with "Every name below is a deliverable" and gives WP-3 an explicit escape hatch — "— (register amendment; reviewed, not tested)". These three IDs get neither a test nor the marker.

`C-014` is the substantive one: `Default = Api` is what makes the ADR's "existing announce users: nothing changes by default" true, and the `api`/`git` spellings are CLI grammar, an interface under `CLAUDE.md`'s stability tiers. `validate_transport_refuses_github_git` exercises the enum's behaviour and would stay green if the default flipped or a spelling changed to `rest`. `C-003` and `C-032` are genuinely doc-only and only need the marker, but `C-032` corrects two statements the ADR calls "factually wrong" on a shipped file, and an untracked doc rewrite is the kind of item that gets dropped from a `panel`-reviewed 9-contract package.

**Fix:** Add `write_transport_default_is_api` and `write_transport_value_spellings` to WP-5's unit row. Mark `C-003` and `C-032` in the Test inventory with the WP-3 escape hatch — "doc-only; reviewed, not tested" — and add them to their packages' review checklists so the reviewer, not a test, is named as the control.

---

### F-15 · Warn · `S-005`'s preserved announce-79 signal is not in the inventory, and the existing pin is not cited

**Where:** UX scenarios, `S-005` and `S-037`; Test inventory, WP-9 and WP-13

**Claim:** "**S-005** | `ocx package announce` on an unclaimed namespace | Unchanged: `UnclaimedNamespace`, **79** — the signal a release wrapper branches on is preserved". "**S-037** | A script branches on exit codes | 79 from `announce` means 'claim first' …".

**Problem:** No named test in the inventory asserts it. The repository does pin it — `unclaimed_namespace_classifies_as_not_found` in `crates/ocx_lib/src/announce/error.rs`, over `Self::UnclaimedNamespace { .. } => Some(crate::cli::ExitCode::NotFound)` with `NotFound = 79` — but the plan never names it, and `S-005`'s whole point is that this work must not disturb it. WP-10 rewrites `announce.rs`'s `NonFastForward` match (`C-056`) and WP-12 rewrites announce's argument grammar (`C-062`); a preservation claim with no cited guard is a claim nobody will check.

**Fix:** Cite the existing test in the WP-10 row — "regression guard: `announce::error::unclaimed_namespace_classifies_as_not_found` (existing) must stay green" — and add `::test_announce_unclaimed_namespace_exits_79` to WP-13's acceptance list so `S-037`'s cross-command exit-code story is asserted end to end rather than in two unrelated halves.

---

### F-16 · Warn · Exit 80 (a write mode with no credential) has no named test on either command

**Where:** UX scenarios `S-001`, `S-014`; Test inventory, WP-13 and WP-14

**Claim:** `S-001`'s error cases — "no credential → 80"; `S-014`'s — "outside a job with no variable → 80".

**Problem:** An ADR Validation item asks for both arms and neither has a home:

> **Credential selection**: `api` with no token → `AuthError`; … `git` with no variable outside a job → `AuthError`; …
> — ADR § Validation

WP-13's sixteen tests cover 64, 65, 69 and 79 but not 80; WP-14's cover 75, 77, 86 and 0 but not 80. The condition is new for `claim` (the ADR's exit table routes it to a "CLI boundary check"), and it is the first thing a publisher hits when they run the command by hand before exporting a token — the headline manual path.

**Fix:** Add `::test_no_credential_exits_80` and `::test_transport_git_outside_job_with_no_variable_exits_80` to WP-13 and WP-14 respectively, and state in `C-063` that the ladder's terminal rung produces `AuthError` (80) naming `OCX_ANNOUNCE_TOKEN` for every write mode while `--out` proceeds unauthenticated.

---

### F-17 · Warn · The stderr owner line has no contract and no test, yet its provenance word must agree across three surfaces

**Where:** UX scenarios `S-001`, `S-008`; Component contracts § Claim library (absent)

**Claim:** `S-001` — "stderr carries the `login:id` owner line". `S-008` — "`owner_identity_source: "ci-environment"`; the stderr line and the request body both say so".

**Problem:** The scenarios oblige it; no `C-###` defines it and no named test asserts it. The ADR makes it a three-surface agreement:

> The same word is rendered in the stderr owner line and in the request body.
> — ADR § the `--format json` report, `owner_identity_source`
> The resolved `login:id` pairs are additionally written to stderr as a status line **before the write**, so a human running the command by hand sees exactly who they are claiming for.
> — ADR § Plain rendering

Three surfaces that must carry one word, with a contract for exactly one of them (`C-046`'s `owner_identity_source` field). The "before the write" ordering is the same shape as `C-064`'s pre-existing-token warning, which *did* get a contract and a test.

**Fix:** Add a contract — "**C-0xx** — the resolved `login:id` pairs and the `owner_identity_source` word are written to stderr before any write; the same word appears in the report and in the request body" — and one test asserting all three renderings agree from a single run (`owner_identity_source_agrees_across_stderr_report_and_body`, WP-9 or WP-11).

---

### F-18 · Warn · `C-063` drops the ADR's "non-empty" qualifier from both credential ladders

**Where:** Component contracts § CLI, `C-063`

**Claim:** "**C-063** — the API credential precedence is `OCX_ANNOUNCE_TOKEN`, then `CI_JOB_TOKEN` **only** when `--transport git` is selected and `GITLAB_CI` is set, then none."

**Problem:** The ADR qualifies every rung on emptiness:

> 1. `OCX_ANNOUNCE_TOKEN`, **when set and non-empty**.
> 2. `CI_JOB_TOKEN`, **only** when `--transport git` is selected, `GITLAB_CI` is set and `CI_JOB_TOKEN` is **non-empty**.
> — ADR § API credential precedence

Under C-063's wording an `OCX_ANNOUNCE_TOKEN=""` exported by a wrapper wins rung 1, the job-token pickup never fires, and `C-026` then maps the empty value to "no header" — a silently unauthenticated run inside a CI job that had a perfectly good `CI_JOB_TOKEN`. That is a plausible CI shape (`export OCX_ANNOUNCE_TOKEN="${SOME_UNSET_VAR}"`), and the empty-credential path is otherwise a supported mode (`C-026`, `S-011`), so nothing else catches it.

**Fix:** Restore "set and non-empty" to both ladders in `C-063`, and add `empty_token_falls_through_to_job_token_pickup` to WP-11's unit row.

---

### F-19 · Warn · `C-047` never says how `name` is derived

**Where:** Component contracts § Claim library, `C-047`; Test inventory, WP-9 (`root_field_order_matches_fixture`)

**Claim:** "**C-047** — the root renderer emits, in this order: `name`, `repository`, `owners`, `status`, … `status` is always `"active"`, `deprecated_message` and `desc` are `null`, `tags` is always `{}`, `created` is a **date** not a timestamp."

**Problem:** Every field's *value* is contracted except the first. The ADR derives it:

> `name` | `<default registry>/<namespace>/<package>` | The index's logical prefix comes from the resolved default registry (`OCX_DEFAULT_REGISTRY`, default `ocx.sh`) — the same source `announce` uses for `Identifier::with_domain`. **No new flag.**
> — ADR § Data Model — the claim root

A tester writing `root_field_order_matches_fixture` from `C-047` alone cannot tell whether the fixture should read `"ocx.sh/acme/widget"` or `"acme/widget"`, and would have to read `announce`'s code to find out — which is the failure the "testable projection" framing of this section exists to prevent. `repository`'s "verbatim, after an `oci://host/path` parse" is likewise implied only through `C-057`.

**Fix:** Add to `C-047`: "`name` is `<resolved default registry>/<namespace>/<package>`, from the same source `announce` uses for `Identifier::with_domain` (`OCX_DEFAULT_REGISTRY`, default `ocx.sh`) — no new flag; `repository` is `--repository` verbatim after the `oci://host/path` parse."

---

### F-20 · Warn · `C-051`'s `Ahead`-byte-identical-without-a-request arm is specified only for `git`

**Where:** Component contracts § Claim library, `C-051`

**Claim:** "**C-051** — … `Ahead` byte-identical **with** an open request → no write at all; `Ahead` byte-identical **without** one → **refresh commit under `git`**; `Ahead` differing → `FastForward` …".

**Problem:** The transport carve-out leaves the `api` arm of that state undefined, and `C-051` is the sole input to WP-9's `branch_state_machine_table` test. The ADR states the state's behaviour transport-neutrally — "`Ahead` with byte-identical content reports `unchanged` **and ensures the request**" (D-C7) — with the refresh commit being `git`'s *mechanism* for ensuring it (D-T4), not a different outcome. As written, a builder implementing the table has no row for `api` + `Ahead` + identical + no open request, which is a reachable state (an interrupted earlier run that pushed but failed to open).

**Fix:** Restate the arm transport-neutrally: "`Ahead` byte-identical without an open request → the request is ensured; under `api` that is the REST open, under `git` a refresh commit (`C-042`)." Ensure `branch_state_machine_table` enumerates both transports for every state.

---

### F-21 · Warn · WP-2's glob defeats the merge-time file-set check it is validated by

**Where:** Work packages, WP-2 Expected files; Merge plan

**Claim:** "`crates/ocx_lib/src/oci/index.rs`, `crates/ocx_lib/src/oci/index/*.rs` (the accessor's new home), `crates/ocx_lib/src/announce/pipeline.rs`".

**Problem:** WP-2 is the only row whose file set is a glob rather than a list, and the parenthetical concedes the destination is undecided. The merge plan validates each branch with "`git diff --name-only <base>..<wp-branch>` against the Expected Files column" — a check that cannot fail against `oci/index/*.rs`, so WP-2's merge gate is structurally green (the unchecked-green shape the plan's own § Red/green discipline names). It also weakens the disjointness claim: any future package touching `oci/index/` collides by construction.

**Fix:** Name the accessor's destination file explicitly (the ADR says the accessor "moves **down** into `oci::index`", so `crates/ocx_lib/src/oci/index.rs` alone may suffice) and drop the glob. If a new submodule is genuinely needed, name it.

---

### F-22 · Warn · The website build is not in the Verification commands table

**Where:** § Verification commands; ADR § Validation, "Gates"

**Claim:** The table's terminal row is "Full gate, before the PR | `task verify --force`".

**Problem:** The ADR's Validation asks for three gates — "**Gates**: `task verify` on ocx; **the website build**; the index-repo docs sweep reviewed" — and the website build is not one of them here. `task verify` does not cover it: `taskfile.yml`'s `verify` runs `.verify:lint` (`rust:format:check`, `rust:clippy:check`, `shell:verify`, `ci:verify`, `claude:verify`, `test:lint`) then `.verify:build-test` (`rust:license:*`, `rust:build`, `rust:test:unit`, `test:parallel`); the website lives behind a separate `website:` taskfile namespace. WP-15 adds a new page under `website/src/docs/user-guide/`, a new cast, and edits to three existing pages — a broken VitePress link or a missing cast asset would reach the PR unbuilt.

**Fix:** Add a row — "Website build (WP-15 gate) | `task website:build`" — and name it as WP-15's Implement gate, since the per-package phase pattern currently says only "`task rust:verify` for Rust packages; the named pytest invocation for Python packages" and WP-15 is neither.

---

### F-23 · Warn · The seeded-branch fixture that reaches `Ahead` and `Diverged` is not a named WP-4 deliverable

**Where:** Work packages, WP-4; Test inventory, WP-4 row

**Claim:** WP-4's five named tests are `::test_clone_over_http`, `::test_push_delivers_four_options`, `::test_post_receive_records_after_delay`, `::test_chunked_receive_pack_body_decoded`, `::test_rejection_hook_writes_literal_refusal_line`.

**Problem:** The ADR specifies two fixture configurations, and only the first is reflected:

> The bare repository starts **without the claim branch**, so the ordinary happy path exercises the `Absent` state … **A second fixture seeds the branch to reach the `Ahead` and `Diverged` arms.**
> — ADR § Test fixture

Four WP-14 tests need the seeded variant — `::test_unchanged_path_with_open_request_performs_no_push`, `::test_unchanged_path_without_open_request_makes_refresh_commit`, `::test_announce_spent_branch_carries_tag_delta_forward`, `::test_moved_target_refetch_and_second_push_succeeds`. WP-4 sits in wave 1 precisely so it is not the critical path; discovering a missing configuration in wave 6 puts it back on it, which is the risk row the wave-1 scheduling exists to close.

**Fix:** Name the seeded configuration in WP-4's scope and add `::test_seeded_branch_fixture_reaches_ahead_and_diverged` to its test row.

---

### F-24 · Warn · `S-027`'s negative half is not asserted, leaving a one-sided warning test

**Where:** UX scenarios, `S-027`; Test inventory, WP-14 (`::test_pre_existing_token_warning_emitted`)

**Claim:** `S-027` — "One stderr line **before the write**, naming the push credential kind and the authoring identity | **the same run outside `GITLAB_CI` emits nothing**."

**Problem:** Only the positive half is a named test. The ADR asks for both — "emits the stderr line naming the push credential kind, before any write; **the same run without `GITLAB_CI` does not**" — and the reason is structural: a test that only asserts the warning appears cannot distinguish the specified conditional from an implementation that warns unconditionally, which would put a spurious line on every non-CI announce. This is `quality-core.md` § Unchecked Green applied to a warning rather than a check. `C-064`'s "before the write" ordering is likewise unasserted.

**Fix:** Add `::test_pre_existing_token_warning_absent_outside_gitlab_ci` to WP-14, and assert ordering by requiring the line in the shim's capture stream before the first `git push` record.

---

### F-25 · Suggest · WP-3 sits below the overhead floor with no one-line justification

**Where:** Work packages, WP-3; § Under-parallelization justification

**Claim:** "**WP-3** | Register amendments — S1 at its source, D0 restatement, D1 operation count | `.claude/artifacts/design_spec_announce_initiative.md`, `.claude/artifacts/adr_announce_gitlab_forge.md` | S | 1 | — | self | scoped".

**Problem:** `protocol.md`: "**The counterweight: no WP below its own overhead.** … A WP whose whole scope is a single trivial concern (~≤50 expected lines) **folds into its nearest sibling as sequential steps**; keeping it isolated needs a one-line justification, checked by the plan review." WP-3 is three sentence-level edits to two markdown files — a worktree, a branch, a review and a merge for well under 50 lines. The plan's § Under-parallelization justification addresses only the *opposite* concern (waves holding a single package) and never this one. It may well be the right call — the amendment is what makes the design constitutionally legal and it is genuinely file-disjoint from everything — but the justification is required and absent.

**Fix:** Add one line to § Under-parallelization justification: why the register amendment stays isolated rather than folding into WP-5 or WP-15 (which already owns `.claude/` documentation surfaces).

---

### F-26 · Suggest · WP-1 carries `light` on the one-way half of the change

**Where:** Work packages, WP-1

**Claim:** "**WP-1** | Exit code 86 + error category. C-001…C-004; S-037 | … | S | 1 | — | **light** | scoped".

**Problem:** `light` fits `protocol.md`'s "single-area moderate change" and the four named tests are strong. But the plan's own Classification names this package's output as one of four reasons the whole change is one-way — "exit code 86 and its `error.kind` value" — and the ADR lists it among five genuinely irreversible items alongside "bytes in an already-merged root". A cheap review on the one-way piece, while the reversible internals (WP-5's trait) get `panel`, inverts the risk ordering.

**Fix:** Either raise WP-1 to `panel`, or record the reasoning in the paragraph that already justifies `Verify: full` on three packages — the wildcard-free `from_exit_code` match makes the omission a compile error, and the frozen `error_kind` inventory test makes the spelling a test failure, so the mechanism substitutes for reviewer attention here in a way it does not elsewhere.

---

### F-27 · Suggest · The new doc script's filename does not follow the tree's slug convention

**Where:** Work packages, WP-15 Expected files; § Documentation surfaces

**Claim:** "`test/doc_scripts/claiming-a-namespace.sh` (**new**)" for the page `website/src/docs/user-guide/claiming-a-namespace.md`.

**Problem:** `test/doc_scripts/` names nested pages with a double-underscore slug — `getting-started__env.sh`, `in-depth__signing.sh`, `entry-points__example-install.sh` — reserving flat names for top-level pages (`deps.sh`, `index.sh`). `test/src/doc_binding.py`'s `_TRANSCLUSION_RE` supports "nested slug paths (``a/b``) and legacy flat forms (``a__b``)", and `test_doc_scripts_cast.py` CA2 records that `# cast: true` plus `# doc: a/b` writes `<casts_dir>/a/b.cast` nested while no `# doc:` writes `<stem>.cast`. The plan specifies neither the `# doc:` header nor a nested name, so a literal execution produces a top-level slug for a `user-guide/` page.

**Fix:** Name it `test/doc_scripts/user-guide__claiming-a-namespace.sh`, or keep the flat filename and specify the `# doc: user-guide/claiming-a-namespace` header in the Documentation surfaces row so the cast lands at `website/src/public/casts/user-guide/claiming-a-namespace.cast`.

---

## Checks that passed

- **Status block** (check 12) — carries all four Plan Status Protocol fields (`Plan`, `Active phase`, `Step`, `Last update`) **and** all four hex fields (`State`, `Tier`, `Updated`, `Next`). `Step: /hex-plan → plan-approved` is a valid hex-family spelling per `meta-ai-config.md`. `Last update: 2026-09-05 (initialized)` deviates from the `(after <sha>: <subject>)` form, which is correct for a plan with no commits yet.
- **CHANGELOG discipline** — the plan never proposes editing `CHANGELOG.md`, states the prohibition explicitly, and carries the release-note lines as commit subjects. The six subjects match the ADR's five (WP-1, WP-10, WP-11, WP-12 ×3) with `!` on the same two.
- **Batched-window carve-out** — `C-062` satisfies every term in `CLAUDE.md`: hidden variant, warn once on stderr and never on stdout, removal release named (0.7), one file (`deprecated.rs`) deleted whole, one window per release pair. DV-3 correctly establishes that no field-level precedent exists.
- **Deviations table** — all six DVs are real deviations, each argued at its point of use; DV-1's premise (that `utility/child_process.rs` holds no spawn primitive) is verified — the file is 135 lines and its module doc points at `launch/child_process.rs`; `SPAWN_ALLOWED` and `no_process_spawn_outside_launch` both exist in `launch.rs`. DV-5's and DV-6's reasoning holds. Only DV-4 misidentifies its gate (F-05).
- **Exit-code arithmetic** — `85 = UnsupportedKeyBackend`, `84 = ReferrersUnsupported`, `79 = NotFound` confirmed in `crates/ocx_lib/src/cli/exit_code.rs`; 86 is the first free slot as claimed.
- **DAG and critical path** (check 7) — recomputed independently, both correct.
