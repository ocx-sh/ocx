# Plan-Artifact Scope

Reference for `/codex-adversary` when invoked with `--scope plan-artifact --target-file .claude/state/plans/plan_<feature>.md` (typically by `/swarm-plan`). Contract differ from default code-diff scope.

## Target

Plan / ADR / system-design markdown file, not git diff. Pass file path to companion so it see full plan contents.

## Triage (simplified — no auto-fix batch)

Plans edited, not compiled, so full auto-fix batch not apply.

| Class | Action |
|---|---|
| **Actionable** | Orchestrator (caller — `/swarm-plan`) edits plan artifact, then re-runs single `worker-reviewer` (focus: `spec-compliance`) pass to validate edit. |
| **Deferred** | Added to Deferred Findings in `/swarm-plan` handoff summary — needs human design judgment. |
| **Stated-convention** | Drop. Mention count only. (Codex critiques load-bearing project convention fixed in `CLAUDE.md` / `AGENTS.md`.) |
| **Trivia** | Drop. Mention count only. (Wording, formatting, markdown style.) |

## One-shot only

No looping. Mirror "prevent two-family stylistic thrash" rationale from code-diff pass.

## Unavailable path

If `CLAUDE_PLUGIN_ROOT` unset or companion returns non-zero, log `Cross-model plan review skipped: <reason>` and return — caller (`/swarm-plan`) decides whether to treat skip as gate miss (max tier) or silent default (lower tiers).