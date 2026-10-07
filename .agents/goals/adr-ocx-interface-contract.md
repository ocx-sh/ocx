# Goal: "The OCX machine-interface contract — one vocabulary, gated documents, generated SDKs"

Source: .claude/artifacts/adr_ocx_interface_contract.md · Written: 2026-10-03 by /hex-loop

## Definition of done

- [ ] "Option D landed: phases 0–1, compat gate, SDK contract; phase-2 Rust type layer decided D vs E at go/no-go" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Grammar: owned cli.json exported from clap with full clap semantics and directional compat rules" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Root convention: every report root an object, schema_version first, list payloads as items" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Errors: v1 document on stdout, pretty-printed, error.detail for every family, remediation removed" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Compat gate: in-house keyword-allowlist differ, unmodelled keywords red, ledger-acknowledged breaks" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Never-null: zero nullable forms in OCX-owned structure, opaque payloads preserved, runtime output validated" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Enum additions additive everywhere, openness published in the schema, status decisions fail closed" — evidence: the commit, PR comment or
  artifact that satisfies it.
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
- Granted acts (authority: the pasted prompt): none
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

- Entry point: Run the /hex-plan skill on "The OCX machine-interface contract — one vocabulary, gated documents, generated SDKs, per .claude/artifacts/adr_ocx_interface_contract.md".
- Refinement rounds: 2 — counts outer cycles: each review ⇄ execute
  pass is one, and so is every failed repair or retry cycle — a local
  verify failure, an execute or finalize retry, a post-finalize CI fix ⇄
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

- Run rules:
  - None.

## Emphasis

None.

## Context

- Source ADR: .claude/artifacts/adr_ocx_interface_contract.md (Status: Proposed)
- System design: .claude/artifacts/system_design_ocx_interface_contract.md
- Origin discussion: .agents/discussions/ocx-interface-contract.md
- Discovery: .claude/artifacts/discover_ocx_interface_contract.md
- Reviews: .claude/artifacts/review_adr_interface_contract_{spec,quality,security,sota,codex}.md
- Post-fix validation (1 Block open: mirror consumer gate cannot go red; 9 Warns): .claude/artifacts/review_adr_interface_contract_validation.md
- Spin-off issues: https://github.com/ocx-sh/ocx/issues/571, https://github.com/ocx-sh/ocx/issues/572
- Loop decisions (2026-10-03): plan .claude/artifacts/plan_ocx_interface_contract.md (310484ef2), ADR Accepted. Other-repo WPs X1–X5 (ocx-mirror, ocx-sdk-python, ocx-sdk-rust) need a grant this run lacks → reported not met; D-17 (phases 1–2 start with cross-repo exit criteria deferred, not waived) accepted. Follow-ups filed: https://github.com/ocx-sh/ocx/issues/573, https://github.com/ocx-sh/ocx/issues/574.
- Loop stop (2026-10-04 07:15): disk floor hit (14 GB free); remaining space is in orphaned ~/.cache/ocx/bazel-root bases, which only the owner may delete. Waves 1–7 are merged at e29d3b906. Wave 8 is partial: WP-23 committed as edc2b880f with 5 review fixes pending; WP-24's last edits are uncommitted in .agents/worktrees/ic-wp-24, and its goldens errors.json and reports.json must be regenerated, not hand-edited. Resume by re-running /hex-loop on this file once disk is at least 25 GB.
- Paused by owner (2026-10-04 12:55) for hand edits: branch at fdc9e352c, waves 1–12 merged (WP-00…27), next WP-28, no worktrees or WP branches left. Resume: base = branch tip as the owner leaves it (owner edits are authoritative), new WP worktrees branch from the tip at dispatch. Lean shape per owner: no per-WP review unless security- or wire-critical, scoped checks in WPs, review per phase, full verify at finalize, heartbeat 20 min or more. Finalize residue: reword subjects bc43cf969, 547aea8de and db4efee7e (`!`); add test_diff_guard --allow lines for the WP-17 and WP-27 renames; owner to decide on frozen reports/v1.json.

