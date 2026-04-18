# Tier: low — /swarm-review

Minimal-effort review for small diffs: flag/option changes, doc edits,
test fixtures, single-subsystem tweaks ≤3 files. The adversarial anchors
still fire (the protocol itself is part of the skill identity) but the
parallel perspective panel shrinks to a single reviewer — no RCA, no
Codex. Matches what a fast branch check against `--base=HEAD~1` or a
close sibling branch should feel like.

Load this file via `Read` from `SKILL.md` after the config is announced.

## Phase 1: Discover

Read the diff against the resolved baseline (already computed by
SKILL.md step 2). Identify the single subsystem touched by matching
changed paths against `.claude/rules.md` "By subsystem". Read that
subsystem's `subsystem-*.md` rule inline. For pure Python tests, read
`subsystem-tests.md`.

**Gate**: Diff fetched, single subsystem identified, context rule read.

## Phase 2: Stage 1 — Correctness (single reviewer)

> **Reviewer model**: the `worker-reviewer` launch in this tier uses the resolved `--reviewer` overlay value (tier=low default `haiku`; escalated to `sonnet` when structural markers from `classify.md:48-61` are present). See `overlays.md` reviewer axis.

Launch **1** `worker-reviewer` (focus: `spec-compliance`, phase:
`post-implementation`) to review the diff. Phase `post-implementation`
because review runs against **Implement-phase output** — there's already
code (not Stub-phase scaffolding, not Specify-phase tests). The
reviewer applies:

- OCX pattern compliance (error model, symlink safety, CLI/API contract)
- Quality (naming, style, tests present, duplication)

The spec-compliance phase anchors cover the Stub / Specify / Implement
lifecycle positions without requiring the reviewer to look at stubs or
specification tests separately — at this tier the entire review
collapses into one pass.

**Gate**: Reviewer completes; findings classified as actionable or
deferred.

## Phase 3: Stage 2 — skipped

Two-Way Door scope. No security, performance, documentation, or
architect perspectives. If the discover phase surprised with signals
the classifier should have caught (e.g., a dependency change slipped
into a "doc" diff), stop and re-run `/swarm-review high <target>` —
don't silently upgrade mid-pipeline.

**Gate**: Skip logged in the output; proceed to verdict.

## Phase 4: Root-cause analysis — skipped

`rca: off`. Reviewer reports findings with proximate cause and
remediation only. If a finding smells systemic the reviewer still flags
it as deferred with a reason — the human can escalate to
`/swarm-review high` or `/architect` for Five Whys.

## Phase 5: Cross-model — skipped

`codex: off`. If the user explicitly passed `--codex`, run the pass
anyway (user override). Otherwise log `Cross-model gate skipped:
tier=low default` and continue.

## Phase 6: Verdict & Output

Produce the review report using the shared skeleton from `SKILL.md`:

```markdown
## Code Review: [target]
### Summary
- Verdict: [Approve / Needs Work / Request Changes]
- Tier: low
- Baseline: <base>
- Diff: N files, +L / -L lines, 1 subsystem
### Stage 1 — Correctness
[Findings with file:line, description, remediation]
### Deferred Findings
[Each with: what it is, why human judgment is needed]
```

Omit Stage 2, Cross-Model, and Root-Cause sections — their absence is
the tier contract, not a bug.

**Gate**: Report printed. No commits (review is read-only).

## Handoff

Standard handoff from `SKILL.md`. Classification line:

```
- Scope: Small (Two-Way Door)
- Tier: low
- Baseline: <base>
- Overlays: breadth=minimal, rca=off, codex=off
```

If actionable findings exist and the caller wants fixes:

    /swarm-execute --target=diff "apply low-tier review findings"
