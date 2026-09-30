# Goal: "Snapshot tag lifecycle: `ocx package prune` and ephemeral index rows"

Source: .claude/artifacts/adr_snapshot_lifecycle.md · Written: 2026-09-29 by /hex-loop

## Definition of done

- [x] ocx package prune per the ADR: explicit and --prerelease/--keep-builds selection, served-root safeguard (81, 75, absent), canonical tag DELETE plus GET confirm, --tags-file append, --dry-run, exit 87 — evidence: the commit, PR comment or
  artifact that satisfies it.
- [x] ocx package announce per the ADR: canonical observation of the given tags only, the removal table, --ephemeral on added rows, sync semantics of --tags-from-registry and --refresh, removed and durable_missing in JSON — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] index bot and schema per the ADR: ephemeral on tagEntry, auto-merge of an ephemeral removal only after canonical MANIFEST_UNKNOWN, durable removals and marker changes to human review — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] other commands per the ADR: index update/sync and --remote drop only ephemeral local rows, cascade repair appends to --tags-file, push writes newline tags files, ocx-mirror carries the marker — evidence: the commit, PR comment or
  artifact that satisfies it.
- [x] every removal decision bypasses caches and [mirrors] — evidence: the commit, PR comment or artifact that
  satisfies it.
- [x] keep-tag guidance documented in the prune, --ephemeral and push --keep-tag help, the command reference and the snapshot-tracks how-to — evidence: the commit, PR comment or artifact that
  satisfies it.
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

- Entry point: Run the /hex-plan skill on "Snapshot tag lifecycle: `ocx package prune` and ephemeral index rows, per .claude/artifacts/adr_snapshot_lifecycle.md".
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

maybe note we should use more sonnet than usual, because it got a lot better

From the source: the ADR's Owner decision log (R1–R6, A–C, S5, Redesign 2026-09-29) is binding, and "ocx is
low-level" governs every doubt — no channels, retention grammar, repoint or linting. The ADR's two Open Questions
(bot registry credential; a pushed-but-never-announced build stays 75) are unanswered: run § Issue resolution on
them, the ADR's recommendations as the default.

## Context

- ADR: .claude/artifacts/adr_snapshot_lifecycle.md (Proposed; WP1–WP8 under "Rollout and documentation")
- System design: .claude/artifacts/system_design_snapshot_lifecycle.md
- Research: .claude/artifacts/research_snapshot_{registry_delete,index_format,deletion_security,ux_prior_art,multistage_promotion}.md, .claude/artifacts/discover_snapshot_lifecycle.md
- Discussion: .agents/discussions/snapshot-lifecycle.md
- Other repos the ADR touches: ocx-sh/index (bot, schema; local checkout /home/mherwig/dev/index), ocx-sh/ocx-mirror

## Decision record

- 2026-09-29 · Q: can WP2's tag DELETE avoid the ungranted `ocx-sh/rust-oci-client` fork change? → research
  (opus): no. Token scopes and the authed request path are private to the fork, and a fork-free DELETE would
  re-own about 200 lines of bearer/Basic auth code (quality-core "Don't Own Non-Domain Code", Block).
  Decision: keep plan DEC-10. The fork commit is authored locally only; the probe (C-004) is built without
  the fork. Pushing `ocx/snapshot-delete` is out of grant, so the "PR merge-ready" and "CI green" criteria
  depend on the owner pushing it.
- 2026-09-30 · Owner grant (in session, verbatim): "if you need changes on our forks just make one pr for each
  touched and push to that pr feature branch. the pr should target ocx/integration". Acted on it: pushed
  `ocx/snapshot-delete` and opened https://github.com/ocx-sh/rust-oci-client/pull/8 → `ocx/integration`.
- 2026-09-30 · Q: can the final full `task verify` run while the shared test registry (`test-registry-1`, zot tmpfs) refuses pushes? → research: a
  private compose project on alternate ports is the only route that avoids the shared stack; starting it, and recreating or probing the
  shared registries, were each denied by the permission layer. Decision: blocked on the owner. Criteria 7–9 (PR, CI, deep verify) wait
  for a registry recreate; 3–4 wait on grants for ocx-sh/index and ocx-sh/ocx-mirror (issues #553, #554).
