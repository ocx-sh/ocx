# Review: adr_index_claim_command — spec, round 3 (re-validation)

**Date:** 2026-09-05
**Author:** reviewer:spec r3 (Opus 5)
**Inputs:**

- `.claude/artifacts/adr_index_claim_command.md` (2123 lines, revised after fix round 2b)
- `.claude/artifacts/system_design_index_claim_command.md` (813 lines, revised)
- `.claude/artifacts/review_adr_index_claim_spec_r2.md` (B1/B2/H1/W2 rulings, N-B1, N-H1..N-H4, N-W1..N-W4, N-S1)
- `.claude/artifacts/review_adr_index_claim_adversary.md` (A-1..A-5 with orchestrator triage; Q1–Q3)
- `.agents/discussions/index-claim-command.md` (read as data — the nine ratified `## Decisions`, Requirements, Verification)
- Code read in this worktree at `487570fb`: `cli/error_category.rs`, `cli/exit_code.rs`, `forge/api.rs`,
  `forge/error.rs`, `forge/gitlab.rs`, `announce.rs`, `announce/error.rs`, `announce/request.rs`,
  `utility/child_process.rs`, `env.rs`, `ocx_cli/src/command/package_announce.rs`,
  `design_spec_announce_initiative.md`, `adr_announce_gitlab_forge.md`

Round-2 sections not named in the disposition list were not re-read.

---

## Disposition of the round-2 fixes

| ID | Claim | Ruling | Anchor | Evidence |
|---|---|---|---|---|
| **A-1** | Recipe step 0 = REST Branches read; second refspec conditional; `Absent` its own state; retry names both refspecs; fixture starts without the branch | **closed** | ADR § "The git recipe" step 0 and "**Step 0 exists because a first claim has no branch.**"; recipe `4r` "*a retry always names both refspecs*"; ADR § Test fixture "*starts **without the claim branch***"; design § 5 "**`Absent` is determined by a REST read, before the workspace exists.**" | Step 0 is `GET /projects/:id/repository/branches/<branch>` with `404 => Absent`; the second refspec carries `# SECOND REFSPEC ONLY WHEN STEP 0 FOUND THE BRANCH`; step 3 is `# SKIPPED ENTIRELY WHEN THE BRANCH IS ABSENT`; the commit parents on `o/<base>` and `<old> = 0{40}`. The design sequence has `GET branches/<branch> (exists? else Absent)` before `ensure workspace`, and the risk row is marked `~~High~~ **closed by design**`. Validation carries the red proof ("*asserting the two-refspec form fails against a repository without the branch*"). One residual: D-C7 never lists `Absent` — see **N2-W5**. |
| **A-2** | `ErrorCategory::ForgeCapabilityUnavailable` (`forge_capability_unavailable`), arm + frozen-inventory row, same phase/step as exit 86, justified by the wildcard-free match | **closed** | ADR § "Exit codes — full mapping", "**A new `ExitCode` variant is not one edit but two, and the second one is a compile error until it is made.**"; ADR Implementation Plan step 1; design § 5 error taxonomy and Phase 1 third bullet | Verified `crates/ocx_lib/src/cli/error_category.rs`: `from_exit_code` runs `:55-79` with no wildcard arm, and the doc comment states the `_ => Internal` history the ADR cites. The frozen tests are real — `error_category_serializes_snake_case` and `error_category_total_over_exit_codes`, the latter pinning `cases.len() == 16`. Both artifacts put the category in the same commit as the code; the ADR also adds the deviation row ("Compelled, not chosen") and Constitution Check §7. |
| **A-3** | `-c credential.helper=` reset scoped to ocx-credential-injecting invocations; step-3 `git-helper` posture keeps helpers, own row + residual sentence; paired fixture assertions | **closed** | ADR § "Push credential precedence", "**Step 3 is the one posture where the helper reset does not apply, and that is deliberate.**"; § Security Architecture §1 "*The scope qualifier is load-bearing rather than a hedge*"; design § 7 row "Git push authentication, **step-3 fallback**" | The recipe preamble matches ("every invocation **that injects an ocx credential** additionally carries `-c credential.helper=`"). Residual risk is stated in both artifacts ("a helper can leak its own credential through its own channels"). Validation asserts both directions: no helper process on a credential-injecting run, and the helper *is* invoked with no `extraHeader` and `push_credential_kind: "git-helper"` on a step-3 run. |
| **A-4** | Every "REST for every read" claim carves out `compare_branch` | **partially closed** | Carved: design § 1 actors table "REST for every read **except `compare_branch`**", § Glossary "Write transport", ADR § Considered Options T-D pro cell "*every read but one*", ADR changed-contracts row "**The one read that is not REST under `git`**". Uncarved: ADR § Considered Options **T-D Description**, "Every read stays REST under both transports"; design § 10 External dependencies, "Forge REST API \| Every read, and the API-transport write" | Two sites still assert the unqualified form. In the ADR the carve-out is the next sentence, so nothing misleads for long; the design's dependency row has no adjacent qualifier at all. See **N2-W4**. |
| **A-5** | Atomicity claim withdrawn; step 6 = 1/2/4/8/15 s poll (~30 s); exhaustion = `MergeRequestUnconfirmed` exit 75 naming the rerun; fixture delays the record | **closed** | ADR D-T4 "**What is and is not atomic — an earlier draft of this ADR overclaimed here.**" and "Atomicity under `git` is **not stronger** than under REST"; recipe step 6 "BOUNDED POLL, not one read"; exit table row "the push succeeded but no merge request appeared within the ~30 s confirmation bound … 75" | The schedule is consistent at all five sites (recipe, the prose under it, the exit-code row, design risk row, design sequence note) and 1+2+4+8+15 = 30. The fixture writes the record "after a configurable delay" and Validation drives both outcomes, refusing a synchronous fixture as evidence. `MergeRequestUnconfirmed` is in the design's `ForgeError` list and in the changed-contracts row. |
| **Q1** | `--upstream-org` anchor, the other two each optional and each `requires` org; deviation row partial; owner `login` regex, `id >= 1`, `minItems: 1` | **closed** | ADR § "API Contract — CLI grammar" three `--upstream-*` rows; mutual-exclusion table "clap `requires`, exit 64" ×2; deviation row "**Partial deviation, now schema-verified.**"; ADR § Open Questions "Owner-field grammar, confirmed against the same schema"; design § 5 `ClaimRequest.upstream` doc and § 6 "Schema constraints" | Both artifacts carry `^[A-Za-z0-9][A-Za-z0-9._-]{0,254}$`, `id >= 1` and `minItems: 1`, and both tie `minItems` to the ladder exiting 64 rather than writing an empty array. Open Questions is `**None.**` in the ADR and `**None open.**` in the design. |
| **B1** | Git contract for `open_or_update_pull_request` with no preceding `commit_files` reaches the design | **closed (diagram defect filed separately)** | design § 5 "Branch state machine (claim)" rows 3 and 4; "**Why a refresh commit exists at all, and why it is not a hack.**"; ADR changed-contracts row "**Except with no pending local commit**" | The byte-identical case is split by open-request existence, with `—` as the ref update for the no-write branch, and the changed-contracts row an implementer copies now carries the exception. The git sequence gained an `alt` block, but its flow still pushes on both arms — the contract is stated correctly in three places and only the diagram disagrees. See **N2-W1**. |
| **B2** | Classifier row promoting a refused push to 86 on the two-signal pair | **closed** | ADR § "The git recipe", stderr-classification table rows 4 and 5; "**Row 4 is what makes the two-signal rule reachable at all.**" | Row 4 is `HTTP 403 … or a remote: line containing not allowed to push / You are not allowed to`, **and** preflight `job-token-push: unknown` → 86; row 5 is the same shapes with `passed` → 77. The ADR states the discriminator explicitly ("the promotion to 86 is driven by the preflight result, not by the phrase"). The fixture pins the literal line `remote: You are not allowed to push code to this project.` and Validation drives all three cases, calling out that "either signal alone reaching 86 is the bug". |
| **H1** | D-C1's mitigation restated as the forge invariant | **closed** | ADR D-C1, "the `ocx index` group's help states that **no index subcommand writes to a forge**" and "The earlier wording — 'index subcommands are local-cache operations' — is withdrawn along with the C-B con it rested on" | The withdrawn sentence is named, the reason is given against the shipped group (`update` fetches tags, `sync` reads live and is refused by `--offline`, `catalog` lists registry repositories, only `regenerate` consults no source), and `quality-cli-help.md`'s "help text is contract text" is cited. The did-you-mean hint is unchanged. |
| **W2** | `adr_announce_gitlab_forge.md:59` "ten operations" scheduled | **partially closed** | ADR § "API Contract — the `Forge` trait", "**The same stale count lives in a second file, and both are scheduled.**"; docs table row `adr_announce_gitlab_forge.md:59` (D1); Implementation Plan step 10; design Phase 1 first bullet | The edit is scheduled in all three places and both artifacts drop the count rather than bump it. But `adr_announce_gitlab_forge.md:59` is an **empty line** — the D1 heading is `:60` and the sentence "`Forge` (`forge/api.rs`) is exactly the ten operations `announce()` drives." is `:62`. Verified the underlying fact: `forge/api.rs` module doc says "ten", the trait declares 11 `async fn`s, and the ADR's "11 → 13" is right. See **N2-W2**. |
| **N-B1** | Version gate at the argv-fault boundary beside `validate_transport`, before the forge exists | **closed for the ordering, open for the plan** | ADR D-T10 "**checked at the argv boundary**"; preflight table `git-version` row; recipe step 1 "RUNS EARLIER THAN THIS LIST"; § "**`git-version` is reported in `capability_checks` but is not part of `ensure_push_access`**"; Security §5; design sequence line 1 "git --version (argv-fault gate, before the forge exists)", § 10 dependency row, § 11 risk row | The contradiction the round-2 finding named is gone: the design sequence now opens with the gate and annotates why, and every ADR assertion of "before any network call" is consistent with it. Two consequences of the move are unhandled — the check has no route into `capability_checks`, and its implementation step precedes the helper it needs. See **N2-H1** and **N2-H2**. |
| **N-H1** | Help text "no index subcommand writes to a forge" | **closed** | Same anchor as H1 | Single sentence, factually true against `ocx_cli/src/command/index.rs`, and stated identically in D-C1's mitigation and in its withdrawal paragraph. |
| **N-H2** | `owner_identity_source` on `ClaimOutcome` + `OwnerIdentitySource { Resolved, Asserted, CiEnvironment }` | **closed** | design § 5 `ClaimOutcome` field `pub owner_identity_source: OwnerIdentitySource`, the enum below it, and "**`owner_identity_source` has to be on the outcome, not derived in the CLI.**" | Spelling is identical across all three surfaces: report key `owner_identity_source` with values `resolved` / `asserted` / `ci-environment`; the enum's doc names the same three wire spellings; the ADR's D-C4 rule 2, the report table and the design's step-2 table all use them unchanged. |
| **N-H3** | Retry widening is its own step 6; plan is ten steps; phase map P1=1, P2=2–3, P3=4–6, P4=7–8, P5=9–10 | **closed** | ADR Implementation Plan step 6, "**Widen announce's non-fast-forward retry to the commit-and-open pair.** Its own step, because nothing else fails when it is skipped"; commit-subject table row 6; design § 12 "Mapping to the ADR's ten steps" and the Phase 3 bullet; design risk row "The retry-scope change is **ADR step 6**" | The plan has exactly ten steps and the phase map covers 1–10 with no gap or overlap. Code citations check out: `announce.rs:292` opens the `match` on `commit_files`, `:307` is the `NonFastForward` arm, and `open_or_update_pull_request` sits at `:404-414` outside it. Downstream renumbering is consistent — step 7 now owns the workspace re-fetch (ADR step 6's own text says so), and both "Implementation Plan step 10" citations (the stale-count sweep, the corporate-CA issue) point at the renumbered docs step. No "nine steps" survives outside the design's round-1 changelog row, which is correct as history. |
| **N-H4** | Both artifacts name the four CA variables; design cites the ADR's allowlist table | **closed** | ADR Security §2, "The issue names **those four CA variables and only those**"; Implementation Plan step 10; design § 7 "Unsupported deployment", "**not** about proxies: `HTTP_PROXY` and its siblings are also allowlisted for the child, but `reqwest` already honours them" | Both name `GIT_SSL_CAINFO` / `GIT_SSL_CAPATH` / `SSL_CERT_FILE` / `SSL_CERT_DIR` verbatim; the design replaced its own list with "The authoritative list is the ADR's child-environment allowlist table — not repeated here, because a second copy is how the two artifacts drifted in the first place." Design Phase 5 repeats the four-CA scoping. |
| **N-W1** | Allowlist paragraph credits the capturing helper, not `spawn_and_wait` | **closed** | ADR Security §4, "**Child environment allowlist.** **The capturing helper** — the same one specified in §4, not `spawn_and_wait`, which is not used on this path — keeps `env_clear()`" | Verified `utility/child_process.rs:138-151`: `spawn_and_wait` calls `env_clear()` but hardcodes all three streams to `inherit` and returns `io::Result<ExitStatus>`, so the ADR's reason for a sibling holds. |
| **N-W2** | Changed-contracts row carries D-T4's no-write branch | **closed** | ADR § "API Contract — the `Forge` trait", changed-contracts table, `open_or_update_pull_request` under `git` | Row now ends "**Except with no pending local commit**, where it first reads the open merge requests over REST and **writes nothing if one exists** … (D-T4). This row is what an implementer copies into the trait doc, so the exception belongs in it rather than only in the decision." |
| **N-W3** | `CheckStatus::Failed` dropped | **closed** | ADR § "API Contract — the `Forge` trait", `pub enum CheckStatus { Passed, Unknown, Skipped }` and "**There is deliberately no `Failed`.**"; design § 3 `api.rs` row "no `Failed`, because no code path can emit one" | Vocabulary sweep over both artifacts: the only `status` values used anywhere in a `CapabilityCheck` context are `passed`, `unknown` and `skipped`, in the preflight table, the report sample and the Validation items. No `failed` survives. |
| **N-W4** | Deviations table names the dossier's second Bearer assertion and the merge-request-URL reversal | **closed** | ADR § "Deviations from the ratified dossier", `PRIVATE-TOKEN` row and the row "The merge-request URL comes from **REST**, not the push's `remote:` lines" | The `PRIVATE-TOKEN` row now reads "Correction of dossier *prose* in **two places, not one**: the Requirements paragraph *and* its Verification bullet". The URL row is new and names the reversal with its reason. |
| **N-S1** | Scheduled `gitlab.rs` range corrected to `:148-160` | **partially closed** | Corrected: ADR docs table row `crates/ocx_lib/src/forge/gitlab.rs:148-160`, design Phase 1 first bullet. Not corrected: ADR § "Industry Context & Research" Key insight, D-T8 "The `gitlab.rs:148-157` comment is still scheduled for correction in full", Implementation Plan step 1 "the whole `forge/gitlab.rs:148-157` comment" | Verified the comment runs `:148-160` in `forge/gitlab.rs` — `:158-160` is the empty-token behaviour. The docs table now argues at length that `:148-157` "stopped three lines short", while the step a builder actually executes still cites `:148-157`. See **N2-W3**. |

---

## New findings

### Block

None.

### High

- **N2-H1 — `capability_checks` has no producer for the `git-version` row, and none at all on announce.**
  Anchor: ADR § "API Contract — the preflight", "**`git-version` is reported in `capability_checks` but
  is not part of `ensure_push_access`**"; ADR § "the `--format json` report", whose sample array opens
  with `{ "name": "git-version", "status": "passed", "detail": "2.43.0" }`; design § 5, `ClaimOutcome`
  and `ClaimRequest`.
  Moving the gate to the CLI argv boundary (the N-B1 remedy) put the check outside the library, but the
  array's only declared carrier is `ClaimOutcome.capability_checks`, filled from the `PushAccess` the
  forge returns. `ClaimRequest` has no field to seed a CLI-side check, and neither artifact says the CLI
  concatenates its own row into the report. The announce half is worse: the ADR says "Announce's report
  gains … `branch`, `capability_checks`", and `AnnounceOutcome`
  (`crates/ocx_lib/src/announce/request.rs:115-140`) carries neither field — verified, it has
  `package`, `status`, `pull_request`, `fork`, `written_paths`, `desc_status`, `reserved_tags_dropped`.
  No step schedules adding them.
  *Failure scenario:* the stub phase compiles the types from the design, phase 4's "Preflight and
  `capability_checks`" wires the three forge-side checks, and `git-version` is silently dropped. The
  ADR's own sample report becomes unproducible and `CapabilityName::GitVersion` — listed in the
  one-way table as an unremovable wire spelling — ships with nothing that emits it, which is the exact
  defect `CheckStatus::Failed` was deleted for one round earlier.
  *Secondary consequence, same field:* the report table says `capability_checks` is "Present and
  non-empty on every run", but the preflight table runs `push-access` only on "every mode that writes"
  and gives a `skipped` rule to the two job-token checks alone. A `--out` run writes nothing and would
  emit an empty array.
  *Fix:* name the carrier. Either add `preflight_checks: Vec<CapabilityCheck>` to `ClaimRequest` (and
  announce's request) so the CLI seeds the argv-boundary result and the orchestration concatenates, or
  state that the CLI assembles the report array from `ClaimOutcome.capability_checks` plus its own row —
  and schedule `branch` and `capability_checks` onto `AnnounceOutcome` in step 5. Give `push-access` a
  `skipped` row for `--out` while there.

- **N2-H2 — the version gate is scheduled two phases before the capturing helper it requires, and no
  step's own checklist owns it.**
  Anchor: ADR Implementation Plan step 7, "The `git --version` gate is **not** here: it lands in step 3
  beside `validate_transport`"; step 3's own text; ADR Security §4, "this recipe needs captured
  **stdout** at four steps (`--version` for the 2.31 gate and the `git-version` check `detail`; …)";
  design Phase 4, "The `git --version` gate lands in **phase 2**".
  Step 3 is the credential model — `ForgeCredentials`, `WriteTransport`, `validate_transport`, the
  `client` signature, header selection, precedence resolution — and its checklist never mentions the
  gate. Step 7 introduces the capturing subprocess sibling in `child_process.rs`. Running `git --version`
  and parsing its first `\d+\.\d+(\.\d+)?` needs captured stdout, which by both artifacts' own reasoning
  only that sibling provides; `spawn_and_wait` is verified at `child_process.rs:138-151` to inherit all
  three streams. So step 3 cannot be built as written, and phase 2's gate ("stubs compile; the REST
  implementations pass the existing fake-forge suite unchanged") passes without it.
  *Failure scenario:* the builder executes step 3 from its checklist, ships no gate, reaches step 7,
  reads "not here", and either back-fills into a merged step or leaves the gate in the workspace — which
  is the placement the ADR spends a full section refusing, and which falsifies the Validation item
  "a missing `git` binary → named error at exit 69 with **zero** network calls recorded".
  *Fix:* list the gate in step 3's own bullet, and move the capturing subprocess helper out of step 7
  into step 3 (or into its own step before it) so the dependency order matches the plan order. Mirror
  both in design Phase 2.

### Warn

- **N2-W1 — the git sequence's `alt` block pushes on the branch that must not push.**
  Anchor: design § 3 "Sequence — the same claim over the git transport", the `alt no pending local
  commit` block and the `GL->>WS: git push -o merge_request.*` arrow that follows its `end`.
  The block has no `else` — the second arm is smuggled in as a message labelled `else: refresh commit`
  — so both arms fall through to the unconditional push, directly contradicting the note inside the
  block ("an open request exists -> return it, NO push at all") and the contract the state machine and
  the changed-contracts row now state correctly. *Fix:* make it a real `alt` / `else` with the REST read
  and the return inside the first arm, and the refresh commit plus the push inside the second.

- **N2-W2 — the scheduled `adr_announce_gitlab_forge.md:59` is a blank line.**
  Anchor: ADR § "API Contract — the `Forge` trait", "**The same stale count lives in a second file**";
  ADR documentation-surfaces row `adr_announce_gitlab_forge.md:59` (D1); design Phase 1.
  Verified: `:59` is empty, `### D1 …` is `:60`, and "exactly the ten operations `announce()` drives"
  is `:62`. A stale anchor that still resolves is the failure mode this project has already been bitten
  by — a diff scoped to the cited line edits nothing and reports done. The row's prose names D1 and the
  count, so the edit is findable; the anchor is not. *Fix:* cite `:62`, or drop the line number and
  anchor on D1's sentence.

- **N2-W3 — three ADR sites still cite the `gitlab.rs:148-157` range the ADR itself calls wrong.**
  Anchor: ADR Implementation Plan step 1, "the whole `forge/gitlab.rs:148-157` comment"; D-T8, "The
  `gitlab.rs:148-157` comment is still scheduled"; § Industry Context Key insight — against the
  documentation-surfaces row, which cites `:148-160` and argues "the earlier `:148-157` stopped three
  lines short of the comment it demands be rewritten whole".
  Step 1 is the instruction a builder follows, and it carries the range the same document has withdrawn.
  *Fix:* `:148-160` at all four sites.

- **N2-W4 — two "every read is REST" claims are still uncarved.**
  Anchor: ADR § Considered Options, T-D **Description**, "Every read stays REST under both transports";
  design § 10 External dependencies, "Forge REST API \| Every read, and the API-transport write".
  The ADR's next sentence carves `compare_branch` out, so the contradiction is one sentence wide; the
  design's dependency row has no qualifier at all, and under `git` the compare genuinely does not touch
  the REST API — which is the row's whole subject. *Fix:* "Every read except `compare_branch` under
  `git`" in both, matching the four sites that already say it.

- **N2-W5 — D-C7 does not enumerate `Absent`, the command's headline state.**
  Anchor: ADR D-C7, "**Claim's branch handling is the simple half of #399**", which enumerates
  `Diverged` / `Behind`, `Identical`, and the two `Ahead` cases — against design § 5 "Branch state
  machine (claim)", whose first row is `Absent`, and the recipe's "`Absent` is its own branch state and
  is **not** `Identical`".
  D-C7 reads as a complete enumeration and is the decision an implementer copies into the state machine;
  the state it omits is the one every first claim takes. *Fix:* add the `Absent` row to D-C7 with its
  ref-update rule, and say what "create" means under `api`, where the design's "`<old>` is all zeros"
  phrasing is git-only and `RefUpdate` (verified at `forge/api.rs:82-87`) has only `FastForward` and
  `Reset`.

### Suggest

- **N2-S1 — two code anchors stop short of the line they name.**
  ADR § "Exit codes — full mapping" cites `announce/error.rs:208-259` for the fall-through, but the
  `_ => None` arm is at `:261` (`:259` is the `OutputWrite` arm). Design § 5 cites `announce.rs:212-233`
  for the unchanged-path ensure where the ADR cites `:211-233`; the `if let` opens at `:211`.
  Neither loses anything today. *Fix:* `:208-262` and `:211-233`.

---

`Blocks remaining: 0 · Highs remaining: 0 · New: 0B/2H/5W/1S`
