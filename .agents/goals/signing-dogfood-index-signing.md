# Goal: "Signing dogfood, index signing — a signed index, Sigstore instance tables and the `ocx.sh` signing job"

Source: .agents/discussions/signing-dogfood-index-signing.md · Written: 2026-11-25 by /hex-loop

## Definition of done

- [x] "Sigstore tables" — evidence: commits `1422ef9d` (a leftover `[trust.sigstore]` is refused with exit 78 `trust_sigstore_retired`, naming the new table), `76d6f706` (`ocx config push` inlines every table's root) and `b2565994` (sign, attest, push and index sign select the table by the Fulcio host); acceptance test `test_a_config_with_the_retired_trust_sigstore_table_fails_every_command_with_78`; loader tests for the system lock and the managed-payload rules; the generated `config.json` schema; `reference/configuration.md` § Sigstore tables. Scope: bullet 1 of the source's `## Requirements` ("Sigstore instance tables.").
- [x] "Signer instance" — evidence: the `sigstore` field in `crates/ocx_trust/src/publishers.rs`, refused on a key entry (`8f645001`); acceptance test `test_a_signer_naming_an_instance_with_no_table_is_a_mismatch_and_reaches_no_network`; an unmatched registries key warns once (`711765b3`); the ADR amendment records that there is no registry-level `sigstore` key. Scope: bullet 2 of the source's `## Requirements` ("Instance named on the signer entry.").
- [x] "ocx index sign" — evidence: `crates/ocx_cli/src/command/index_sign.rs`; acceptance tests `test_key_signing_writes_the_signature_and_sets_expires_thirty_days_out` and the keyless test against the local Sigstore stack; keyless and static-key signing on real Fulcio certificates in https://github.com/ocx-sh/e2e-index/actions/runs/45676941597. Scope: bullet 3 of the source's `## Requirements` ("`ocx index sign`.").
- [x] "Client check" — evidence: `test/tests/test_index_signature.py` (expired catalog, changed root, `--no-verify`, unpinned and pinned managed signer lists, mirror refusal), the `IndexCheckPosture` unit tests, and the `warn` and `enforce` cases on a served index in https://github.com/ocx-sh/e2e-index/actions/runs/45676941597 (41 passed at `c7056f7f`); review round 3 found no code finding above Suggest. Scope: bullet 4 of the source's `## Requirements` ("Client check.").
- [x] "Catalog schema" — evidence: ocx-sh/index `7f0c51e2` (the schema admits `expires`, with a valid and an invalid fixture) in https://github.com/ocx-sh/index/pull/1606; ocx-sh/catalog `3677e48` (`test/sources/walker-signed-catalog.test.ts`) in https://github.com/ocx-sh/catalog/pull/26; the index bot only writes `c/index.json` (`core/render.py`), established by reading the code, with no test on a signed catalog. Scope: bullet 5 of the source's `## Requirements` ("Catalog schema and its readers.").
- [x] "Signing job, alert" — evidence: ocx-sh/index `ea3f676e` (the signing job, its bootstrap pinned in `7735633c`) in https://github.com/ocx-sh/index/pull/1606; ocx-sh/indexbot `484b384`, `ee4b9d1`, `7f4b64a` and `0a7a28b` (the `catalog-expiry` alert and its tested verify script) in https://github.com/ocx-sh/indexbot/pull/12, every check green at `0a7a28b`; job and alert both run on real certificates in https://github.com/ocx-sh/e2e-index/actions/runs/45676941597. `scripts/ocx-sign.pin` in ocx-sh/index is all-zero on purpose and fails closed until the owner pins a released build. Scope: bullet 6 of the source's `## Requirements` ("`ocx.sh` signing job and expiry alert.").
- [x] "sbom deps flag" — evidence: commit `46dd3eb1` (`--dependencies` / `--no-dependencies`, off by default); `test/tests/test_package_sbom_dependencies.py`; `reference/command-line/package-consuming.md`. Scope: bullet 7 of the source's `## Requirements` ("`ocx package sbom` and dependencies.").
- [x] "e2e-index proof" — evidence: https://github.com/ocx-sh/e2e-index/actions/runs/45676941597: 41 passed with ocx at `c7056f7f`, the fixture signed by a dev build of `02d8014d` (signing code unchanged since); keyless and static key under `warn` and `enforce`, expired (81), changed root (65), catalog changed after signing (65), unsigned (79), wrong identity refused, the previous release still installs; fixture at ocx-sh/e2e-index `977c0fb8`. Scope: bullet 8 of the source's `## Requirements` ("End-to-end proof in `ocx-sh/e2e-index`.").
- [x] "Mirror lockstep" — evidence: ocx-sh/ocx-mirror `d82a041` links ocx `c7056f7f` in https://github.com/ocx-sh/ocx-mirror/pull/123 (checks: 14 passed, 2 skipped); `task satellite:verify` exit 0 and `cargo metadata --locked` exit 0 at that pointer; mirror acceptance 231 passed, 3 skipped. Scope: bullet 9 of the source's `## Requirements` ("ocx-mirror lockstep.").
- [x] "Reference docs" — evidence: commit `2370a09a` closes the three gaps review round 3 named (`explanation/signing.mdx` names `index_http_failed` for an oversized catalog; the `OCX_NO_VERIFY` entry lists `index catalog` and `index list`; the `index sync` entry states exit 79 under `enforce`); `task website:reference:check` exit 0; pages `reference/configuration.md` § Sigstore tables, `reference/environment.md`, `reference/command-line/index-commands.md`, `reference/command-line/package-consuming.md` and `explanation/signing.mdx`. Scope: bullet 10 of the source's `## Requirements` ("Reference documentation.").
- [ ] PR merge-ready (DONE block only) — evidence: the PR URL, and the
  PR is not a draft.
- [ ] CI green per job (DONE block only) — evidence: every check run on
  the PR head SHA with its conclusion; every skipped job states its
  skip reason (its `if:` or path filter).
  A skip with no reason is not green.
- [ ] Deep verify passed (DONE block only) — evidence: the full documented verification result.

## Autonomy

- Prompting: never — no question waits for a human; a doubt runs
  § Issue resolution.
- Granted acts (authority: the pasted prompt): You may push the loop's branch to ocx-sh/ocx and open its pull request during the run; You may dispatch release.yml in dev mode on that branch, publishing to ghcr.io/ocx-sh/dev/cli only; In ocx-sh/e2e-index and ocx-sh/e2e-package you may push, force-push, tag, release, change settings and set secrets; In ocx-sh/index, ocx-sh/indexbot, ocx-sh/catalog and ocx-sh/ocx-mirror you may push a branch and open a pull request; Never merge, never cut a production release or tag, never write to ocx.sh or the production index
- Forbidden or narrowed acts: Never merge, never cut a production release or tag, never write to ocx.sh or the production index.

## Issue resolution

Every doubt — an ambiguous requirement, a design question, a failure
with an unclear cause — runs this protocol:

1. Delegate the research to a sub-orchestrator.
2. Record question → research → decision in this file or in the PR.
3. Defer to a GitHub issue only in hard cases — the research ends with no
   decision, or the decision needs an act outside the grants; the issue
   link is the recorded decision.
4. Every doubt not deferred ends in an action.
5. A pre-existing failure that blocks done is in scope and runs this
   protocol.
6. Findings outside this goal's scope become follow-up issues.
7. Secrets and credential-bearing logs are never written to any committed
   file, commit message, PR text, PR comment or review comment, or issue.
   Security findings are never filed as issues:
   this file records them by reference only — location and class, no
   secret value or exploit detail — and only the DONE block reports them
   in full.

### Recorded decisions

Each entry is question → research → decision, per step 2. The plan
(`.claude/artifacts/plan_signing_dogfood_index_signing.md`) holds the design
decisions of each pass in full; this list holds the ones the run took instead
of asking.

- **Branch base.** The source discussion was not on `main` → it exists only in
  `b8f644e8` on `duo` → the branch is cut from `origin/main` and that commit is
  cherry-picked.
- **Satellite checkouts.** `indexbot` and `e2e-index` had no local checkout →
  both were cloned; every satellite is worked in a worktree or its own clone,
  never in a checked-out tree that carries someone else's work.
- **How the end-to-end proof is started.** No dispatch is granted in
  `ocx-sh/e2e-index` → its `client-e2e.yml` runs on a push to the loop branch,
  and the fixture is signed on its `main` by a fast-forward push (granted).
- **Production signing pin.** A dev build can be pruned and production acts are
  the owner's → `scripts/ocx-sign.pin` in ocx-sh/index stays all-zero and fails
  closed until the owner pins a released build.
- **ocx-mirror toolchain.** The released `ocx` lacks `--sigstore` → the mirror's
  toolchain `ocx` is pinned to a pull request build; it must be re-pinned before
  that pull request merges.
- **`OCX_SIGSTORE_TRUSTED_ROOT` and the catalog check.** Owner decision,
  2026-11-26 → only `ocx package verify` and `ocx package sbom` read the
  variable; the catalog check and install-time verification take each root
  from its table, the convention file, the cache, then TUF. An offline host
  supplies the public root through the convention file.
- **Roots committed before `enforce`.** A root stored under `warn`, `off` or
  `--no-verify` is not re-checked after a switch to `enforce` → the fix reopens
  plan decision P-D3 → deferred to the work that turns `enforce` on for
  `ocx.sh`; the limit and the recovery steps are documented.
- **Unpinned managed configuration.** It may only tighten → an `index` value
  that removes or reroutes the index source of an armed registry is stripped
  with a warning.
- **Mirror refusal under `enforce`.** Refusing at start-up blocked commands that
  read no index → the refusal (exit 78) fires at the first index read of that
  registry, and `--no-verify` / `OCX_NO_VERIFY` clear it.
- **`index catalog --remote`.** Plan S-208 says `--verify` wins wherever the
  check runs → it carries the `--verify` / `--no-verify` pair.
- **Unwritable rollback record.** Failing every command on a read-only state
  directory is worse than the lost protection → the client warns and continues.
- **Third dev build.** Signing code is unchanged since `02d8014d` → no third
  build; the fixture stays signed by the build of `02d8014d`.
- **Red checks after the last fix pass.** CI at `c7056f7f` is red on a missing
  license header, the closing review asks for one documentation fix, and
  ocx-sh/indexbot#12 is red on a type check → § Loop shape counts "a red gate's
  fix pass" as an outer cycle, and so does the finalize contract's own fix pass
  → both cycles are spent, so nothing is fixed, `/hex-finalize` is not run, and
  the remaining work is ocx-sh/ocx#780.
- **"Signing job, alert" and "Reference docs" stay unticked.** The alert's pull
  request fails its own gate, and three reference pages state or omit an exit
  code → both are reported `not met` with the exact gap.
- **Non-blocking findings.** → ocx-sh/ocx#781 (review leftovers) and
  ocx-sh/ocx#782 (an accessibility check already red on `main` of
  `ocx-sh/catalog`).
- **Record commits are not pushed.** A push re-runs the whole pipeline for a
  change to this file and the plan only → the review record and the ticks stay
  local on the branch.
- **Past both caps, on the owner's word.** The owner answered the first DONE
  block with "fix and finalize" (2026-11-25) → the items of ocx-sh/ocx#780 are
  fixed (`927a29eb`, `2370a09a`; ocx-sh/indexbot `0a7a28b`), "Signing job,
  alert" and "Reference docs" are ticked, and `/hex-finalize` runs. No fourth
  review ran: the fix is the one sentence review round 3 named, two reference
  gaps and a license header.
- **Finalize from this checkout.** The finalize pre-flight refuses a linked
  worktree, and its fix (switch the branch in the first worktree) cannot work:
  that checkout holds `main` and this one holds the branch → this is the
  owner's long-lived checkout, where the session opened the branch, not a tree
  under `.agents/worktrees/`; three earlier runs decided the same → the run
  goes on.
- **`.serena/project.yml`.** A tool rewrites this tracked file and the run may
  neither commit nor revert it, so the tree is never clean → the file is marked
  skip-worktree for the finalize run and the mark is cleared after it; its
  content is untouched and enters no commit.
- **One pipeline for the fixes.** → the fixes are not pushed ahead of finalize;
  its force-push starts the one run on the final commit, and the pull request
  is flipped to ready only after that run is green.
- **No deep verify dispatch.** `hex.md › Preferences` rules out a
  `verify-deep.yml` dispatch unless the owner asks, and the pasted prompt's
  "Done when" text grants nothing → not dispatched; the pull request's own
  pipeline is the remote gate.

Security findings, by reference only (location and class):

- `crates/ocx_sign/src/verify/trust_resolve.rs` — scope of a trust-root
  override; ordering of a cache write against the verdict. Fixed.
- `crates/ocx_index/src/ocx_index.rs` — reuse of an unchecked value by a
  checked read; a second, unchecked fetch after a failed one; URL redaction.
  Fixed; one redaction residual.
- `crates/ocx_config/src/loader.rs` — coverage of the managed-configuration
  guard. Fixed for the catalog check; three older residuals outside it.
- The rollback record under `state/index_catalog/` — behaviour on a write
  error; key collision. Fixed; one residual on the stored bound.
- `crates/ocx_config/src/terminal.rs` — coverage of the terminal escaper.
  Residual.
- `crates/ocx_cli/src/command/package_push.rs` — a local write for a refused
  namespace. Residual.
- `render-deploy.yml` in ocx-sh/index — pinning of a bootstrap action in a job
  that holds an identity token. Fixed.
- By design: roots stored before `enforce`; a mode downgrade under `warn`.
  Closed 2026-11-26: the authority of `OCX_SIGSTORE_TRUSTED_ROOT` on its host,
  for installs and the catalog check, which no longer read it.

## Loop shape

- Entry point: Run the /hex-plan skill on "Signing dogfood, index signing — a signed index, Sigstore instance tables and the `ocx.sh` signing job, per .agents/discussions/signing-dogfood-index-signing.md".
- Refinement rounds: 2 — counts outer cycles: each review ⇄ execute
  pass is one, and so is every failed repair or retry cycle — a red
  gate's fix pass, an execute or finalize retry, a post-finalize CI fix ⇄
  re-finalize pass. Inner review-fix rounds do not count, and their limit
  is untouched.
  Past `2`, the DONE block reports every remaining criterion `not met`
  and the run stops.
- Ticks: /hex-loop commits nothing. The session creates or switches to
  the branch the pasted prompt's I9 names and commits this file first on
  it. A box is ticked only between hex-mode runs, never during one, and
  each tick is committed at once; every tick lands before the final
  /hex-finalize, none after it. The `(DONE block only)` criteria are
  evidenced only in the closing DONE block, never ticked.

## Rules

None.

## Emphasis

From the source, as pointers; the source's own text governs.

- Each criterion's scope is the whole numbered bullet of the source's `## Requirements`, not its short title.
- Order: the Sigstore instance tables come first, because the keyless `ocx.sh` index signature cannot be verified on a host pinned to a private instance without them. The pull request order is an open question with a default. The signing job must have run on `ocx.sh` before a client with the `warn` default is released, and that release is the owner's act.
- Non-breaking as the source's `## Intent` defines it, with one granted exception: `[trust.sigstore]` is replaced as a hard break, refused by name, with no second form and no window.
- The source's `## Intent` lists what is out of scope: `enforce` on `ocx.sh`, the Sigstore TUF mirror, a signature on the managed configuration, Rekor v2, every mirror feature, the index bot's `publishers` rules and the catalog badges, and every production act.
- The source's `## Decisions` are closed positions. Two depart from the ADR's item 4 (no registry-level `sigstore` key; `ocx index sign` signs keylessly too), and the run amends the ADR.
- The source's `## Open questions` are this run's docket. Each carries a default the owner confirmed as the working answer; the plan overturns one only with evidence, and reports it.
- The source's `## Verification` names the acceptance cases; each is shown red on the failing case and green on the passing one. Cases that need a real certificate or a served index run in `ocx-sh/e2e-index`, dispatched, never per pull request.
- The source marks what was not read or measured: its `Confidence:` line, Requirements bullet 5 (the index bot and the catalog renderer), and Decisions › Access checked.
- Decisions › Loop shape describes the phases the owner wants (integrate, end-user break attempts, coarse reviews). The cycle cap is this file's `Refinement rounds:` line and the pasted prompt, not the numbers given there.
- The second loop reads the real names this run settles (the signature's file name, the configuration keys, the `ocx index sign` flags, the library entry point that signs an index) from the ADR and the plan. Record them there; `.agents/discussions/signing-dogfood-mirror.md` opens with a "Start here" block that looks for them.
- New commands, flags and configuration keys are interface changes (`cli.json`, the generated schema, the reference pages, the contract ledger where a gated document breaks).

## Context

- Source discussion: `.agents/discussions/signing-dogfood-index-signing.md`. It is committed in `b8f644e8` on branch `duo` and was not on `main` when this file was written; where the working tree lacks it, read it with `git show b8f644e8:.agents/discussions/signing-dogfood-index-signing.md`.
- Sibling discussions: `.agents/discussions/signing-dogfood-mirror.md` (the second loop; its Threads hold the discussion behind the decisions), `.agents/discussions/signing-dogfood-index-catalog.md` (parked, not part of this run). Both are in the same commit.
- Design this builds: `.claude/artifacts/adr_publisher_trust_distribution.md` § Sequencing, item 4; `.claude/artifacts/system_design_publisher_trust.md` § 8.
- Related decisions: `.claude/artifacts/adr_offline_verify_trust_cache.md`, `.claude/artifacts/adr_real_sigstore_stack_and_delegation.md`, `.claude/artifacts/adr_index_indirection.md`, `.claude/artifacts/adr_trust_policy.md`.
- Research: `.claude/artifacts/research_multi_instance_sigstore_config.md`, `.claude/artifacts/research_mirror_signature_practice.md`, `.claude/artifacts/research_artifactory_generic_remote_caching.md` (same commit).
- Previous loop of the effort: `.agents/goals/signing-dogfood.md`.
- Repositories: https://github.com/ocx-sh/index, https://github.com/ocx-sh/indexbot, https://github.com/ocx-sh/catalog, https://github.com/ocx-sh/ocx-mirror, https://github.com/ocx-sh/e2e-index, https://github.com/ocx-sh/e2e-package. Which of them have a local checkout, and where, is in the source's Decisions › Access checked; the `indexbot` and `catalog` federation rows of `.agents/memory/hex.md` name paths that did not exist on 2026-11-25.
- Related issue: https://github.com/ocx-sh/ocx/issues/107.
