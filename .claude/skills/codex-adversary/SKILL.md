---
name: codex-adversary
description: Use when the user says "codex review", "/codex-adversary", "adversarial review", "cross-model review", or asks for a different-model second opinion on a diff or plan artifact. Complements `/swarm-review` via Codex-CLI. Scopes: code-diff (default), plan-artifact (gate for `/swarm-plan`).
user-invocable: true
disable-model-invocation: true
argument-hint: "[--wait|--background] [--base <ref>] [--scope auto|working-tree|branch|plan-artifact] [--target-file <path>] [focus text]"
triggers:
  - "codex review"
  - "adversarial review"
  - "cross-model review"
  - "second opinion"
  - "another model"
---

# /codex-adversary — Cross-Model Adversarial Review + Triage

Run Codex as adversarial reviewer (different model family →
different blind spots than Claude), then **auto-triage** findings and
drive Claude implementation of approved ones.

## When to Use This vs `/swarm-review`

| Skill | Reviewer | When |
|---|---|---|
| `/swarm-review` | Claude sub-agents (multi-perspective: security, perf, architecture, SOTA) | Fast intra-family multi-angle review. Same training data, same blind spots. |
| `/codex-adversary` | Codex CLI (GPT-5.x family) via plugin runtime | Cross-model second opinion. Slower. Catches things Claude family miss. |

Use both on big work — complement each other. `/swarm-review`
first (cheaper, faster), then `/codex-adversary` for
independent cross-model challenge before merge.

## Context for Codex

Codex reads `AGENTS.md` at repo root every invocation. OCX
`AGENTS.md` already contains:

- Tech stack and golden-path choices
- Path → subsystem rule map (so Codex consults
  `.claude/rules/subsystem-*.md` before flagging)
- Language-level quality rule pointers (`.claude/rules/quality-*.md`)
- Hard-rule conventions (commits, git workflow, code patterns)
- Security attack surfaces
- Review guidance framing adversarial pass

**Do not** re-inject this context in review prompt — Codex already loads
it. Pass extra focus text only when user wants to steer review
to specific concern.

## Workflow

### 1. Scope detection

Two scope families, caller picks:

- **code-diff** (default; scopes `working-tree` / `branch` / `--base`)
  — reviews git diff. Used by `/swarm-execute` and direct user
  invocation.
- **plan-artifact** (scope `plan-artifact`, `--target-file <path>`) —
  reviews plan / ADR / design markdown file. Invoked by
  `/swarm-plan` after Claude review panel converges. See
  [`references/plan-artifact-scope.md`](./references/plan-artifact-scope.md).

For code-diff scopes, run in parallel:

- `git status --porcelain=v1`
- `git diff --shortstat` and `git diff --shortstat --cached`
- `git log -5 --oneline`
- `git rev-parse --abbrev-ref HEAD`

Pick review scope:

- **working-tree** (default): unstaged + staged + untracked changes
- **branch**: `main..HEAD` — everything since branch diverged from main
- **--base `<ref>`**: user-supplied base
- **plan-artifact**: file path via `--target-file` is
  target; skip git probes.

If user passed focus text as argument, treat as intent (e.g.,
"security review of the new archive extractor") and pass through to
Codex unchanged. No rewrite.

### 2. Run the review via the plugin runtime

Use Codex companion script directly — same entry point plugin
`/codex:adversarial-review` command uses. Keeps Codex context
loading, auth, runtime identical to first-party command.

```bash
node "${CLAUDE_PLUGIN_ROOT}/scripts/codex-companion.mjs" adversarial-review "$ARGUMENTS"
```

`CLAUDE_PLUGIN_ROOT` env var set by Claude Code harness when
Codex plugin loaded. If unset (plugin missing), stop and tell
user to install/enable Codex plugin.

Execution mode:

- `--wait` in args → foreground
- `--background` in args → Claude background Bash task
- Neither → estimate size. Small (1–2 files, no directory-wide change): ask
  via `AskUserQuestion`, two options (`Wait for results (Recommended)`
  vs `Run in background`). Bigger: default background, tell
  user check `/codex:status`.

### 3. Auto-triage (Claude's job)

When review returns, Claude processes Codex free-text output and
makes **triaged report**. No passthrough-prompt user for every
finding — filter smart.

See [`references/triage.md`](./references/triage.md) for full triage
class table (Auto-fix / Needs confirmation / Discuss / Filtered-trivia /
Filtered-stated-convention / Filtered-false-positive), heuristics for
"safe to auto-fix", and triaged report format.

### 4. Implementation (auto-fix items)

For each auto-fix item:

1. Read affected file(s) to confirm Codex description matches reality
2. Apply fix with `Edit` (not `Write`)
3. Run targeted checks — `cargo check -p <crate>` for Rust, relevant
   `task lint:*` for markdown/config, etc.

After all auto-fixes:

4. Run `task verify` once (not per file)
5. Update report with outcome of each auto-fix (✅ applied /
   ⚠️ reverted-and-promoted / ❌ failed)

Do **not** commit. Commit always user explicit call — follow
`/commit` conventions when invoked.

### 5. Needs-confirmation items

After auto-fix batch done, walk through *needs confirmation* items
one at a time using `AskUserQuestion` with Apply / Skip / Modify options.
Apply approved fixes same way as auto-fix items (`Edit` + targeted
check + final `task verify`).

### 6. Discuss items

Present verbatim at session end. Claude may add own
read per item, but no forceful advocate — these user decisions.

## Flags

- `--wait` — foreground execution, show review as soon as returns
- `--background` — run review as Claude background Bash task, return
  immediately; user checks `/codex:status` then asks for triage
- `--base <ref>` — review `ref..HEAD` instead of working tree
- `--scope auto|working-tree|branch|plan-artifact` — explicit scope
- `--target-file <path>` — required when `--scope plan-artifact`;
  plan/ADR markdown file to hand to Codex
- `--no-auto-fix` — skip auto-fix batch; treat every real finding as
  *needs confirmation*. Use on sensitive code or when
  want see everything before change lands.
- `--review-only` — run Codex review, dump raw output, skip triage
  entirely. Equivalent to running `/codex:adversarial-review` directly.

## Plan-artifact scope (invoked by /swarm-plan)

When called with `--scope plan-artifact --target-file
.claude/state/plans/plan_<feature>.md`, contract changes — triage
simplified (no auto-fix batch; plans edited, not compiled) and
pass one-shot. See [`references/plan-artifact-scope.md`](./references/plan-artifact-scope.md)
for full contract.

## Failure modes

See [`references/failure-modes.md`](./references/failure-modes.md) —
Codex unavailable, no changes to review, empty/garbled Codex output,
`task verify` failures after auto-fix.

## References

- `AGENTS.md` — project context Codex loads every invocation
- `/codex:adversarial-review` — first-party plugin command; this skill
  wraps with OCX-specific triage
- `/swarm-review` — Claude-intra-family multi-perspective review
- `.claude/rules/quality-core.md` — anti-pattern severity definitions
  (Block / Warn / Suggest) used in triaged report
- `.claude/skills/builder/SKILL.md` — implementation pattern for
  applying approved fixes
- [`references/triage.md`](./references/triage.md) — classes, heuristics, report template
- [`references/plan-artifact-scope.md`](./references/plan-artifact-scope.md) — plan-artifact contract
- [`references/failure-modes.md`](./references/failure-modes.md) — error paths