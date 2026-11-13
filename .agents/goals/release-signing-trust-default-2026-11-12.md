# Goal: "Package signatures checked by the client, declared in a signed index"

Source: .agents/discussions/release-signing-trust-default-2026-11-12.md · Written: 2026-11-13 by /hex-loop

## Definition of done

- [x] "Index entry declares publishers" — evidence: `test/tests/test_publisher_trust.py` `test_s005_*` (unrecognised-only list refused, usable entry beside one installs) and the `ocx_index` / `ocx_trust::publishers` unit tables, green in gate run 10 (`task verify:scoped --force`, exit 0, tree d9edf62f4); shape in `.claude/artifacts/system_design_publisher_trust.md`. Scope: the index package file carries `publishers` in the `[[trust.policy]]` signer vocabulary, any one match passes; an entry with an unknown kind or field (the `sigstore` field included for now) matches nobody; no understood entry refuses.
- [x] "Client verifies against publishers" — evidence: `test/tests/test_publisher_trust.py` `test_s001_*`, `test_s002_*`, `test_s003_*` (pass, wrong signer exit 77, unsigned exit 79), `test_s010_*` (installed and lock-pinned exec make no index request, also under `OCX_FROZEN` and `OCX_OFFLINE`), `test_s016_*` (locked package in a fresh home), `test_s017_*` (content installed under another name is verified before a declared exec), green in gate run 10 (`task verify:scoped --force`, exit 0, tree d9edf62f4). Scope: install, pull and on-demand installs check the package signature against `publishers` with no local configuration; committed local resolves, `--offline`, `--frozen` and `ocx.lock` make no request.
- [x] "Per-registry signatures setting" — evidence: `test/tests/test_publisher_trust.py` `test_s004_*`, `test_s008_*`, `test_s015_*` (default installs undeclared, `required` refuses exit 81, `off` skips, unknown value behaves as `required`), green in gate run 10 (`task verify:scoped --force`, exit 0, tree d9edf62f4). Spelling is `if_declared` (plan decision C-009, recorded reason: the config file's snake_case values); `website/src/content/docs/reference/configuration.md`. Scope: `signatures = "if-declared"` (default), `"required"` (a missing `publishers` fails), `"off"` (never checks), under `[registries."<host>"]`.
- [x] "Generic claims match on keyless" — evidence: `ocx_trust` and `ocx_sign::verify::identity` unit tests over the recorded GitHub leaf (`test/tests/fixtures/public_leaf/`), green in gate run 10 (`task verify:scoped --force`, exit 0, tree d9edf62f4); public cases `e2e/claims` (names) and `e2e/oid` (raw OIDs) in `test/e2e/test_publisher_trust_public.py`; names checked in `.claude/artifacts/research_fulcio_claim_oids.md`. Scope: a `claims` map keyed by known name or raw OID, values compared as strings, all listed claims must match, an unknown name matches nobody.
- [x] "Key publisher needs no Sigstore network" — evidence: `test/tests/test_publisher_trust.py` `test_s006_key_publisher_installs_with_sigstore_unreachable_and_no_trust_root*` and `test_s006_a_signature_by_a_different_key_is_refused_with_publisher_mismatch`, green in gate run 10 (`task verify:scoped --force`, exit 0, tree d9edf62f4). Scope: a `key` publisher verifies with no Sigstore network access.
- [x] "Publishers and trust.policy both pass" — evidence: `test/tests/test_publisher_trust.py` `test_s007_install_needs_both_the_publisher_and_the_operator_signature`, `test_s007_missing_operator_signature_*`, `test_s007_missing_publisher_signature_*`, green in gate run 10 (`task verify:scoped --force`, exit 0, tree d9edf62f4). Scope: a package covered by `publishers` and by an operator `[[trust.policy]]` installs only when both are satisfied.
- [x] "--no-verify skips the check" — evidence: `test/tests/test_publisher_trust.py` `test_s009_no_verify_installs_an_unsigned_declared_package_with_one_warning` and `test_s009_no_verify_installs_past_a_corrupt_committed_root`, green in gate run 10 (`task verify:scoped --force`, exit 0, tree d9edf62f4). Scope: `--no-verify` and `OCX_NO_VERIFY` skip the check with today's one-line warning.
- [x] "SBOM: publishers apply, newest wins" — evidence: `test/tests/test_publisher_trust.py` `test_s011_*` and `test_s012_*`, green in gate run 10 (`task verify:scoped --force`, exit 0, tree d9edf62f4); public `two-sboms` and attestation cases in `test/e2e/test_publisher_trust_public.py`; contract ledger entry for `package sbom` in `crates/ocx_schema/contract/ledger.toml`. Scope: `publishers` is the signer set for attestations; `ocx package sbom` takes the newest verified SBOM instead of refusing several.
- [x] "Previous ocx release still installs" — evidence: `test/tests/test_publisher_trust_previous.py::test_s013_previous_release_installs_a_package_whose_root_carries_publishers` (drives the published `ocx-previous`), green in gate run 10 (`task verify:scoped --force`, exit 0, tree d9edf62f4). Scope: the previous ocx release installs a package whose entry carries `publishers`.
- [x] "E2E fixtures exist and are exercised" — evidence: https://github.com/ocx-sh/e2e-index/actions/runs/43365086954 (conclusion success, 17 passed, client built from a snapshot of tree d9edf62f4; this branch differs from that tree only in this file and the plan's Status block). Fixtures: `ocx-sh/e2e-package` main 3121466 publishing `ghcr.io/ocx-sh/e2e-package` cases `main`, `two-sboms`, `tag`, `other`, `key`, `unsigned` (runs 43351174093, 43351177213, 43351174136, 43351187199, 43351184159, 43351180883, all success); `ocx-sh/e2e-index` main 21d9fba serving https://ocx-sh.github.io/e2e-index with roots `e2e/claims`, `e2e/oid`, `e2e/tagged`, `e2e/none`, `e2e/future`. Scope: `ocx-sh/e2e-package` publishes the fixed cases signed with real GitHub-issued certificates, `ocx-sh/e2e-index` serves a static index whose entries carry `publishers`, and a dispatched workflow runs the branch-built client against both.
- [x] "ADR, design and reference amended" — evidence: `.claude/artifacts/adr_publisher_trust_distribution.md` (Accepted), `.claude/artifacts/system_design_publisher_trust.md`, `website/src/content/docs/reference/configuration.md` on this branch. Scope: `.claude/artifacts/adr_publisher_trust_distribution.md`, `.claude/artifacts/system_design_publisher_trust.md` and `website/src/content/docs/reference/configuration.md` state this design.
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
- Granted acts (authority: the pasted prompt): May commit to main, force-push, change settings and secrets, and dispatch publish and sign workflows in ocx-sh/e2e-package and ocx-sh/e2e-index
- Forbidden or narrowed acts: None.

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

## Loop shape

- Entry point: Run the /hex-plan skill on "Package signatures checked by the client, declared in a signed index, per .agents/discussions/release-signing-trust-default-2026-11-12.md".
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

- The scope is the first slice only: the source's `## Intent` lists what is out of scope for it, and `## Decisions` › Sequencing lists the later slices. Production `ocx-sh/index`, the real releases of ocx, ocx-mirror and grimoire, and the `indexbot` and `catalog` satellites are not part of this run.
- The source supersedes the Decision Outcome of the ADR where they conflict; TUF metadata over the index is dropped. Config and field spellings in the source are working names, and items it marks as proposed or inferred are open to a better shape with a recorded reason.
- The source's `## Open questions` and spikes are this run's docket, notably the Fulcio claim names and value encoding, which are checked against Fulcio's own OID document before names are fixed, and whether `[[trust.policy]]` keeps its structure.
- The source's `## Threads` are closed positions.
- The source's `## Verification` names the acceptance cases; each is shown red on the failing case and green on the passing one. The local Sigstore stack issues certificates without repository or owner claims, which is why the real-certificate cases come from `ocx-sh/e2e-package`.

## Context

- Source discussion: `.agents/discussions/release-signing-trust-default-2026-11-12.md` (untracked; read it from the main checkout).
- Superseded discussion: `.agents/discussions/release-signing-trust-default.md`.
- ADR and design to amend: `.claude/artifacts/adr_publisher_trust_distribution.md`, `.claude/artifacts/system_design_publisher_trust.md`; requirements background: `.claude/artifacts/prd_release_signing_trust_default.md`.
- Related ADRs: `.claude/artifacts/adr_trust_policy.md`, `.claude/artifacts/adr_offline_verify_trust_cache.md`, `.claude/artifacts/adr_oci_referrers_signing_v1.md`.
- Fixture repositories: https://github.com/ocx-sh/e2e-package and https://github.com/ocx-sh/e2e-index (public, README only when this file was written).
- Related issues: https://github.com/ocx-sh/ocx/issues/392, https://github.com/ocx-sh/ocx/issues/403.
