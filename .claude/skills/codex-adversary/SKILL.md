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

Run Codex as an adversarial reviewer (genuinely different model family →
different blind spots than Claude), then **auto-triage** the findings and
drive Claude implementation of the approved ones.

## When to Use This vs `/swarm-review`

| Skill | Reviewer | When |
|---|---|---|
| `/swarm-review` | Claude sub-agents (multi-perspective: security, perf, architecture, SOTA) | Fast intra-family multi-angle review. Same training data, same blind spots. |
| `/codex-adversary` | Codex CLI (GPT-5.x family) via plugin runtime | Cross-model second opinion. Slower. Catches things Claude's family systematically misses. |

Use both on substantial work — they complement each other. `/swarm-review`
first (it's cheaper and faster), then `/codex-adversary` when you want the
independent cross-model challenge before merge.

## Context for Codex

Codex reads `AGENTS.md` at the repo root on every invocation. OCX's
`AGENTS.md` already contains:

- Tech stack and golden-path choices
- A path → subsystem rule map (so Codex knows to consult
  `.claude/rules/subsystem-*.md` before flagging)
- Language-level quality rule pointers (`.claude/rules/quality-*.md`)
- Hard-rule conventions (commits, git workflow, code patterns)
- Security attack surfaces
- Review guidance framing the adversarial pass

**Do not** re-inject this context in the review prompt — Codex already loads
it. Only pass additional focus text when the user wants to steer the review
to a specific concern.

## Workflow

### 1. Scope detection

Two scope families, selected by the caller:

- **code-diff** (default; scopes `working-tree` / `branch` / `--base`)
  — reviews a git diff. Used by `/swarm-execute` and direct user
  invocation.
- **plan-artifact** (scope `plan-artifact`, `--target-file <path>`) —
  reviews a plan / ADR / design markdown file. Invoked by
  `/swarm-plan` after the Claude review panel converges. See
  [`references/plan-artifact-scope.md`](./references/plan-artifact-scope.md).

For code-diff scopes, run in parallel:

- `git status --porcelain=v1`
- `git diff --shortstat` and `git diff --shortstat --cached`
- `git log -5 --oneline`
- `git rev-parse --abbrev-ref HEAD`

Decide the review scope:

- **working-tree** (default): unstaged + staged + untracked changes
- **branch**: `main..HEAD` — everything since branch diverged from main
- **--base `<ref>`**: user-supplied base
- **plan-artifact**: the file path passed via `--target-file` is the
  target; skip git probes.

If the user passed focus text as argument, treat it as intent (e.g.,
"security review of the new archive extractor") and pass it through to
Codex unchanged. Do not rewrite it.

### 2. Run the review via the plugin runtime

Use the Codex companion script directly — same entry point the plugin's
`/codex:adversarial-review` command uses. This keeps Codex's context
loading, auth, and runtime identical to the first-party command.

```bash
node "${CLAUDE_PLUGIN_ROOT}/scripts/codex-companion.mjs" adversarial-review "$ARGUMENTS"
```

The `CLAUDE_PLUGIN_ROOT` env var is set by the Claude Code harness when the
Codex plugin is loaded. If it is not set (plugin missing), stop and tell the
user to install/enable the Codex plugin.

Execution mode:

- `--wait` in arguments → foreground
- `--background` in arguments → Claude background Bash task
- Neither → estimate size. Small (1–2 files, no directory-wide change): ask
  via `AskUserQuestion` with two options (`Wait for results (Recommended)`
  vs `Run in background`). Anything bigger: default to background and tell
  the user to check `/codex:status`.

### 3. Auto-triage (Claude's job)

When the review returns, Claude processes Codex's free-text output and
produces a **triaged report**. Do not passthrough-prompt the user for every
finding — filter intelligently.

See [`references/triage.md`](./references/triage.md) for the full triage
class table (Auto-fix / Needs confirmation / Discuss / Filtered-trivia /
Filtered-stated-convention / Filtered-false-positive), the heuristics for
"safe to auto-fix", and the triaged report format.

### 4. Implementation (auto-fix items)

For each auto-fix item:

1. Read the affected file(s) to confirm Codex's description matches reality
2. Apply the fix with `Edit` (not `Write`)
3. Run targeted checks — `cargo check -p <crate>` for Rust, the relevant
   `task lint:*` for markdown/config, etc.

After all auto-fixes:

4. Run `task verify` once (not per file)
5. Update the report with the outcome of each auto-fix (✅ applied /
   ⚠️ reverted-and-promoted / ❌ failed)

Do **not** commit. Committing is always the user's explicit call — follow
`/commit` conventions when they invoke it.

### 5. Needs-confirmation items

After the auto-fix batch completes, walk through *needs confirmation* items
one at a time using `AskUserQuestion` with Apply / Skip / Modify options.
Apply approved fixes the same way as auto-fix items (`Edit` + targeted
check + final `task verify`).

### 6. Discuss items

Present these verbatim at the end of the session. Claude may add its own
read per item, but must not advocate forcefully — these are user decisions.

## Flags

- `--wait` — foreground execution, show the review as soon as it returns
- `--background` — run the review as a Claude background Bash task, return
  immediately; user checks `/codex:status` and then asks for triage
- `--base <ref>` — review `ref..HEAD` instead of working tree
- `--scope auto|working-tree|branch|plan-artifact` — explicit scope
- `--target-file <path>` — required when `--scope plan-artifact`; the
  plan/ADR markdown file to hand to Codex
- `--no-auto-fix` — skip the auto-fix batch; treat every real finding as
  *needs confirmation*. Use when working on sensitive code or when you
  want to see everything before any change lands.
- `--review-only` — run the Codex review, dump the raw output, skip triage
  entirely. Equivalent to running `/codex:adversarial-review` directly.

## Plan-artifact scope (invoked by /swarm-plan)

When called with `--scope plan-artifact --target-file
.claude/state/plans/plan_<feature>.md`, the contract changes — triage is
simplified (no auto-fix batch; plans are edited, not compiled) and the
pass is one-shot. See [`references/plan-artifact-scope.md`](./references/plan-artifact-scope.md)
for the full contract.

## Failure modes

See [`references/failure-modes.md`](./references/failure-modes.md) —
Codex unavailable, no changes to review, empty/garbled Codex output,
`task verify` failures after auto-fix.

## References

- `AGENTS.md` — project context Codex loads on every invocation
- `/codex:adversarial-review` — first-party plugin command; this skill
  wraps it with OCX-specific triage
- `/swarm-review` — Claude-intra-family multi-perspective review
- `.claude/rules/quality-core.md` — anti-pattern severity definitions
  (Block / Warn / Suggest) used in the triaged report
- `.claude/skills/builder/SKILL.md` — the implementation pattern for
  applying approved fixes
- [`references/triage.md`](./references/triage.md) — classes, heuristics, report template
- [`references/plan-artifact-scope.md`](./references/plan-artifact-scope.md) — plan-artifact contract
- [`references/failure-modes.md`](./references/failure-modes.md) — error paths
