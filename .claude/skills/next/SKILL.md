---
name: next
description: Use when user asks "what's next" or "next step". Prints next slash command from current state via a sonnet subagent. Flags: `--clear`, `--list`.
user-invocable: true
disable-model-invocation: true
argument-hint: "[--clear | --list]"
triggers:
  - "what's next"
  - "what is next"
  - "next step"
  - "next command"
  - "what now"
---

# /next — Suggest the Next Slash Command

Look at current state (branch, working tree, plans, recent commits, open PR). Print exactly **one** slash command user copy-paste to continue. State inspection delegated to **sonnet subagent** so output stay compact, main session context not flooded with `git log` / `gh pr view` / plan-file output.

**Primarily read-only.** Becomes a one-shot **state-fixer** when no `## Status` block found and no `.claude/state/current_plan.md` pointer — prompts user to confirm inferred state, then writes `current_plan.md` + injects Status block into plan so future invocations land in the fast path. Never edits code, never commits, never pushes.

## Flags

- `--clear` — tell subagent make suggested command **fully self-contained** (absolute paths, no "the work we just did" refs), prepend `/clear` so future session with no memory run cleanly.
- `--list` — subagent return top 3 candidates with one-line rationale each.
- (no flag) — one self-contained command + one-line "why" + one-line "what was checked".

## Workflow

### 1. Parse flags

Strip `--clear` and `--list` from argument string. Leftover = free-text intent (rare; e.g. `/next --clear after fixing the bug`).

### 2. Delegate to sonnet subagent

Spawn **one** subagent (`Agent` tool, `subagent_type: general-purpose`, `model: sonnet`) with prompt below. Do NOT run inspection commands yourself — point of delegation = keep main context clean.

**Subagent prompt template** (fill `{flags}` and `{intent}` placeholders):

```
You are inspecting OCX project state to suggest exactly ONE next slash command for the user to type. You are read-only — never edit, commit, or push.

Flags from the user: {flags}     # e.g. "--clear", "--list", or "" (none)
Free-text intent (optional): {intent}

## Step 1 — Snapshot state (parallel batch, single message, multiple Bash calls)

- `git rev-parse --abbrev-ref HEAD`               # current branch
- `git status --porcelain=v1`                     # never -uall
- `git log -10 --oneline`                         # commit window
- `git rev-list --count main..HEAD 2>/dev/null`   # commits ahead of main
- `git diff --stat main...HEAD 2>/dev/null`       # changed-files summary
- `gh pr view --json number,state,baseRefName,headRefName 2>/dev/null` # open PR for branch
- `cat .claude/state/current_plan.md 2>/dev/null`                           # pointer to active plan (primary signal)
- `ls -t .claude/state/plans/*.md 2>/dev/null | head -10`              # recent plans (mtime)
- `ls -t .claude/state/plans/meta-plan_*.md 2>/dev/null | head -5`     # recent meta-plans

## Step 2 — Resolve active plan (Status block is primary signal)

Resolution order — first match wins. **Steps 1+2 are read-only fast paths. Step 3 is the state-fixer fallback that prompts the user and persists their answer to `.claude/state/current_plan.md` so future invocations land in step 1.**

### Step 2a — `.claude/state/current_plan.md` pointer (preferred)

If `current_plan.md` exists, parse it for the `**Plan:**` line. If the referenced plan file exists and has a `## Status` block (grep `^## Status$` to next `^## ` heading), use the block's `Active phase` + `Step` fields verbatim. Done — go to Step 3.

### Step 2b — Status block on most-recent plan

If `current_plan.md` absent, glob `.claude/state/plans/plan_*.md` (mtime desc). For each, grep for `## Status` block. First match wins — use its fields. Don't read the whole plan, just the first 30 lines.

### Step 2c — Inferred-state fallback (state-fixer)

If steps 2a+2b both fail (legacy plan with no Status block, or stale state, or no plan file matches), fall back to **commit-subject heuristic**:

1. Identify newest plan via mtime: `ls -t .claude/state/plans/plan_*.md`
2. Grep `^## Phase` headers in that plan to enumerate phases
3. Grep commit subjects in `main..HEAD` (`git log --format=%s main..HEAD`)
4. Cross-reference: a phase is **done** if its title (or commit-subject keywords from it, e.g. "load_exclusive", "ocx lock") appears in any commit subject. The first phase with no commit-subject match is **active**.
5. **Prompt the user via `AskUserQuestion`** with the inferred state:

       Inferred from commits + plan headers: plan=<X>, phase=<N> "<title>" active.
       Confirm and persist to .claude/state/current_plan.md?

   Options:
   - **Confirm + persist** (default) — write `current_plan.md`, inject `## Status` block at top of plan file (after H1, before next heading). Future `/next` invocations land in Step 2a.
   - **Different phase** — user names phase number; same persistence.
   - **No active plan** — write nothing, treat as Step 2d.

If `--list` flag set, do not prompt — present the inferred phase as candidate #1 and let user pick.

### Step 2d — No active plan (valid empty state)

If user picks "No active plan" in Step 2c, or no plan files exist at all:

- `commits-ahead-of-main = 0` AND clean tree → say so explicitly, suggest browsing open issues/PRs (`gh issue list --state open --limit 5` or `/swarm-plan "<task>"` for new work)
- `commits-ahead > 0` AND no plan → suggest `/swarm-plan` to capture the in-flight work as a tracked plan, OR `/commit` if there's a Checkpoint
- Right after `/finalize` clears `current_plan.md` → "feature just landed, pick next PR/issue"

Empty state is a **valid outcome**, not an error.

## Step 3 — Classify the phase

Apply the first matching row. **Status-block fields beat heuristic table** when both apply.

| Observed state                                                          | Suggest                                       |
|---|---|
| Status `Step: /swarm-execute → ...` OR plan-approved OR phase done with next phase | `/swarm-execute <tier> <plan-path>` |
| Status `Step: /swarm-review → round N`                                  | `/swarm-review` (continue same round)         |
| Status all phases done, dirty tree                                      | `/commit`                                     |
| Status all phases done, clean tree, no PR                               | `/swarm-review` then `/finalize`              |
| On `main`                                                               | `/swarm-plan "<task>"` — never work on main   |
| Branch exists, no plan, no commits ahead                                | `/swarm-plan "<task>"` (ask if no task named) |
| Meta-plan exists, no `plan_*.md` yet                                    | Awaiting plan approval — print `/swarm-plan`  |
| Plan exists, 0 commits ahead, clean tree                                | `/swarm-execute <plan-path>`                  |
| Plan exists, commits ahead, dirty tree                                  | `/commit`                                     |
| PR exists (state OPEN)                                                  | `/swarm-review #<N>`                          |
| Worktree branch (goat/evelynn/sion/soraka), multiple Checkpoints, clean | `/finalize` (only if Status confirms phases done) |
| HEAD subject == `Checkpoint`, clean tree                                | `/commit` (will draft + amend)                |

When two rows tie, pick the earlier (correctness > convenience). **Never suggest `/finalize` without confirming via Status block that no phases are still active** — that was the regression this skill is fixing.

## Step 4 — Synthesize the command

Rules — strictly enforced when `--clear` is set; recommended otherwise:

- Use absolute paths to plan artifacts (`.claude/state/plans/plan_X.md`).
- Inline tier and overlay flags from the plan's handoff block when known (`/swarm-execute max .claude/state/plans/plan_X.md`).
- For `/swarm-review`, prefer a PR number (`#61`) when an open PR exists; otherwise the branch name.
- For `/commit` and `/finalize`, no arguments — they snapshot state themselves.
- For `/swarm-plan`, include a 1-line task description in quotes.
- NEVER reference "the previous work" / "what we just discussed" / "the bug from earlier".
- NEVER suggest a slash command that is not present in `.claude/rules.md` "Skills by task topic" — read that table if uncertain.

## Step 5 — Reply

Output exactly one of these three formats. No preamble, no signature.

DEFAULT (no flag):
~~~
Next:
    <slash-command>

Why:    <one-line reason>
Looked at: <one-line list of state checks>
~~~

--clear:
~~~
Next (handover — context will be cleared):
    /clear
    <self-contained slash-command>

Why:    <one-line reason>
Looked at: <one-line list of state checks>
Note: command is self-contained — safe to run after /clear.
~~~

--list:
~~~
Candidates:
  1. <slash-command>
       <one-line rationale, why this is #1>
  2. <slash-command>
       <one-line rationale>
  3. <slash-command>
       <one-line rationale>
~~~

## Edge cases

- No git repo → reply: "not in a git repo — nothing to suggest". Exit.
- Detached HEAD → suggest `git checkout <branch>` first; do not propose a swarm skill.
- `gh` unavailable / unauthenticated → skip PR detection, fall through to branch baseline.
- Multiple plans with mtime within 1h → list them, ask the user to pick (still one Candidates block).
- Skill list expanded recently → re-read `.claude/rules.md` "Skills by task topic" before mapping. Never invent a skill.

Keep your reply under 12 lines. The user will copy-paste from your output directly.
```

### 3. Print subagent's output verbatim

Subagent already formatted output. Do not rewrap, summarise, comment. Print exactly what returned. If `--clear` set, subagent output already include `/clear` line — do not add another.

If subagent report hard error (no git repo, detached HEAD, etc.), print message and stop.

## Constraints

- NEVER edit code, commit, push.
- May write `.claude/state/current_plan.md` and inject `## Status` block into a plan **only via user-confirmed Step 2c state-fixer path**. Never silently mutate state. The user's `AskUserQuestion` answer is consent.
- NEVER run inspection commands in main session — subagent's job. Skip delegation = defeat context-isolation.
- NEVER suggest slash command not in `.claude/skills/`. Subagent cross-checks `.claude/rules.md`; main session no second-guess.
- ALWAYS spawn exactly one subagent. No parallelism — only one question asked.
- NEVER suggest `/finalize` without confirming via Status block that all phases are marked done. (See Step 3 table — premature finalize was the regression that motivated this skill's redesign.)

## References

- `.claude/rules.md` — "Skills by task topic" (authoritative skill list — subagent reads it)
- `.claude/rules/workflow-intent.md` — work-type router
- `.claude/rules/workflow-git.md` — branching, two-phase commit/finalize
- `.claude/rules/meta-ai-config.md` "Plan Status Protocol" — schema + per-skill mutation table; `.claude/state/` tree is gitignored (per-worktree)
- `.claude/skills/swarm-execute/SKILL.md` "Next Step" — handoff pattern this skill emulates