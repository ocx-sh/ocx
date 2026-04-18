---
name: codex-adversary
description: Run an adversarial cross-model code review via Codex CLI, auto-triage the findings (filter noise, classify safe-auto-fix vs needs-confirmation), and hand approved fixes to the builder pattern. Use when you want a genuinely different model's perspective on your changes — not another Claude agent. Complements /swarm-review (which is multi-perspective but Claude-intra-family). Two scope modes — code diff (default, branch/working-tree) and plan-artifact (invoked by /swarm-plan against a plan/ADR file as a final gate). Use when the user says "codex review", "/codex-adversary", "adversarial review", or asks for a cross-model second opinion.
user-invocable: true
disable-model-invocation: true
argument-hint: "[--wait|--background] [--base <ref>] [--scope auto|working-tree|branch|plan-artifact] [--target-file <path>] [focus text]"
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
  `/swarm-plan` after the Claude review panel converges.

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

#### Triage classes

| Class | Definition | Action |
|---|---|---|
| **Auto-fix** | Clear root cause, small edit, obviously correct, no architectural or security implications, fits existing patterns, covered by test suite | Apply directly via `Edit` / `Write`. Run `task verify`. Report what was done. |
| **Needs confirmation** | Correct observation but the fix has design implications, touches security/auth, introduces or removes a dependency, changes a public API, affects a large surface | Present the finding + proposed fix + tradeoff. Wait for user decision via `AskUserQuestion` or free-text. |
| **Discuss** | Codex raises a real design question with no single right answer (e.g., "this approach assumes X — is that intentional?") | Present the question verbatim with Claude's take. Let the user decide. Do not auto-answer. |
| **Filtered — trivia** | Subjective style, nit-pick formatting (`cargo fmt` handles it), wording in comments | Drop. Mention count only. |
| **Filtered — stated convention** | Codex critiques something explicitly fixed by `AGENTS.md` / `CLAUDE.md` (e.g., "consider Nix instead of OCI", "consider async-std instead of Tokio", "add Co-Authored-By") | Drop. Mention count only. |
| **Filtered — false positive** | Codex flags something based on stale or partial context; verify against current code before counting it | Drop. If you are not sure it is false, promote to *needs confirmation* instead. |

**Heuristics for "safe to auto-fix"**:

- The fix is ≤10 lines and affects ≤3 files
- The fix is entirely inside one subsystem (no cross-crate changes)
- No security/auth/secrets/crypto code touched
- No `pub` API surface changed
- No new dependencies, no version bumps
- No changes to `.claude/`, `CLAUDE.md`, `AGENTS.md`, or quality gates
- Tests still pass (verify after)

If in doubt, promote to **needs confirmation**. Better to ask once than to
apply a wrong "obvious" fix.

#### Report format

Produce a single triaged report back to the user:

```
## Codex adversarial review — triaged

**Auto-fix** (N): applying directly
1. <one-line summary> — file:line → brief fix description
2. ...

**Needs confirmation** (N): proposing, waiting for decision
1. <finding> → proposed fix → tradeoff
2. ...

**Discuss** (N): design questions
1. <question> → Claude's read

**Filtered** (M trivia, K stated-convention, P false-positive)
```

Then, without further prompting, apply the **Auto-fix** items. If any
auto-fix has non-trivial implications discovered during implementation
(unexpected compile errors, cross-cutting changes), abort that item and
reclassify it as **needs confirmation**.

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
.claude/artifacts/plan_<feature>.md`, the contract changes:

- **Target**: the plan / ADR / system-design markdown file, not a git
  diff. Pass the file path to the companion so it sees the full plan
  contents.
- **Triage** (simplified — no auto-fix batch; plans are edited, not
  compiled):

  | Class | Action |
  |---|---|
  | **Actionable** | Orchestrator (the caller — `/swarm-plan`) edits the plan artifact, then re-runs a single `worker-reviewer` (focus: `spec-compliance`) pass to validate the edit. |
  | **Deferred** | Added to Deferred Findings in the `/swarm-plan` handoff summary — requires human design judgment. |
  | **Stated-convention** | Drop. Mention count only. (Codex critiques a load-bearing project convention fixed in `CLAUDE.md` / `AGENTS.md`.) |
  | **Trivia** | Drop. Mention count only. (Wording, formatting, markdown style.) |

- **One-shot only** — no looping. Mirrors the "prevent two-family
  stylistic thrash" rationale from the code-diff pass.
- **Unavailable path**: if `CLAUDE_PLUGIN_ROOT` is unset or the
  companion returns non-zero, log `Cross-model plan review skipped:
  <reason>` and return — the caller (`/swarm-plan`) decides whether
  to treat the skip as a gate miss (max tier) or a silent default
  (lower tiers).

## Failure modes

- **Codex unavailable** — companion returns non-zero or reports not ready.
  Stop with the companion's diagnostic output and suggest `/codex:setup`.
- **No changes to review** — stop with "nothing to review" after
  double-checking untracked files (Codex can review untracked work).
- **Codex output empty or garbled** — return Codex's stdout verbatim and
  skip triage. Tell the user the review appears empty and ask if they want
  to rerun.
- **`task verify` fails after auto-fix** — revert the failing edit, mark
  that item `⚠️ reverted-and-promoted`, continue with the rest.

## References

- `AGENTS.md` — project context Codex loads on every invocation
- `/codex:adversarial-review` — first-party plugin command; this skill
  wraps it with OCX-specific triage
- `/swarm-review` — Claude-intra-family multi-perspective review
- `.claude/rules/quality-core.md` — anti-pattern severity definitions
  (Block / Warn / Suggest) used in the triaged report
- `.claude/skills/builder/SKILL.md` — the implementation pattern for
  applying approved fixes
