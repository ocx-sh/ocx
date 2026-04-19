# Plan-Artifact Scope

Reference for `/codex-adversary` when invoked with `--scope plan-artifact --target-file .claude/state/plans/plan_<feature>.md` (typically by `/swarm-plan`). The contract differs from the default code-diff scope.

## Target

The plan / ADR / system-design markdown file, not a git diff. Pass the file path to the companion so it sees the full plan contents.

## Triage (simplified — no auto-fix batch)

Plans are edited, not compiled, so the full auto-fix batch does not apply.

| Class | Action |
|---|---|
| **Actionable** | Orchestrator (the caller — `/swarm-plan`) edits the plan artifact, then re-runs a single `worker-reviewer` (focus: `spec-compliance`) pass to validate the edit. |
| **Deferred** | Added to Deferred Findings in the `/swarm-plan` handoff summary — requires human design judgment. |
| **Stated-convention** | Drop. Mention count only. (Codex critiques a load-bearing project convention fixed in `CLAUDE.md` / `AGENTS.md`.) |
| **Trivia** | Drop. Mention count only. (Wording, formatting, markdown style.) |

## One-shot only

No looping. Mirrors the "prevent two-family stylistic thrash" rationale from the code-diff pass.

## Unavailable path

If `CLAUDE_PLUGIN_ROOT` is unset or the companion returns non-zero, log `Cross-model plan review skipped: <reason>` and return — the caller (`/swarm-plan`) decides whether to treat the skip as a gate miss (max tier) or a silent default (lower tiers).
