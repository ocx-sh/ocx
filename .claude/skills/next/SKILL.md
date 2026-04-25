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

Looks at the current state (branch, working tree, plans, recent commits, open PR) and prints exactly **one** slash command the user can copy-paste to continue. The state inspection is delegated to a **sonnet subagent** so the output stays compact and the main session's context is not flooded with `git log` / `gh pr view` / plan-file output.

This is a **read-only advisory skill**. It never edits code, never commits, never pushes.

## Flags

- `--clear` — instruct the subagent to make the suggested command **fully self-contained** (absolute paths, no "the work we just did" references), and prepend `/clear` so a future session with no memory can run it cleanly.
- `--list` — ask the subagent to return the top 3 candidates with one-line rationale each.
- (no flag) — one self-contained command + a one-line "why" + a one-line "what was checked".

## Workflow

### 1. Parse flags

Strip `--clear` and `--list` from the argument string. Anything left is free-text intent (rare; e.g. `/next --clear after fixing the bug`).

### 2. Delegate to sonnet subagent

Spawn **one** subagent (`Agent` tool, `subagent_type: general-purpose`, `model: sonnet`) with the prompt below. Do NOT run any of the inspection commands yourself — the whole point of delegation is to keep the main context clean.

**Subagent prompt template** (fill the `{flags}` and `{intent}` placeholders):

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
- `ls -t .claude/state/plans/*.md 2>/dev/null | head -10`              # recent plans (mtime)
- `ls -t .claude/state/plans/meta-plan_*.md 2>/dev/null | head -5`     # recent meta-plans

## Step 2 — Identify active plan

In order: (a) plan filename shares a token with the current branch name; (b) most recent meta-plan if newer than matched plan = mid-planning; (c) mtime fallback; (d) no plan = bail to "/swarm-plan" suggestion.

When a plan is found, Read its handoff block (Tier / Scope / Subsystems) and scan its phase checkboxes (`- [ ]` vs `- [x]`) to estimate progress. Don't read the whole plan — just the first 80 lines.

## Step 3 — Classify the phase

Apply the first matching row:

| Observed state                                              | Suggest                                       |
|---|---|
| On `main`                                                   | `/swarm-plan "<task>"` — never work on main   |
| Branch exists, no plan, no commits ahead                    | `/swarm-plan "<task>"` (ask if no task named) |
| Meta-plan exists, no `plan_*.md` yet                        | Awaiting plan approval — print `/swarm-plan`  |
| Plan exists, 0 commits ahead, clean tree                    | `/swarm-execute <plan-path>`                  |
| Plan exists, commits ahead, dirty tree                      | `/commit`                                     |
| Plan exists, commits ahead, clean tree, no PR, looks done   | `/swarm-review` then `/finalize`              |
| PR exists (state OPEN)                                      | `/swarm-review #<N>`                          |
| Worktree branch (goat/evelynn/sion/soraka), multiple Checkpoints, clean | `/finalize`                       |
| HEAD subject == `Checkpoint`, clean tree                    | `/commit` (will draft + amend)                |

When two rows tie, pick the earlier (correctness > convenience).

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

### 3. Print the subagent's output verbatim

The subagent already formatted the output. Do not rewrap, summarise, or comment. Print exactly what it returned. If `--clear` was set, the subagent's output already includes the `/clear` line — do not add another.

If the subagent reported a hard error (no git repo, detached HEAD, etc.), print that message and stop.

## Constraints

- NEVER write files, edit code, commit, or push. Read-only.
- NEVER run the inspection commands in the main session — that's the subagent's job. Skipping delegation defeats the context-isolation purpose.
- NEVER suggest a slash command that does not exist in `.claude/skills/`. The subagent cross-checks `.claude/rules.md`; the main session should not second-guess.
- ALWAYS spawn exactly one subagent. No parallelism — there's only one question being asked.

## References

- `.claude/rules.md` — "Skills by task topic" table (authoritative skill list — the subagent reads it)
- `.claude/rules/workflow-intent.md` — work-type router
- `.claude/rules/workflow-git.md` — branching model, two-phase commit/finalize split
- `.claude/skills/swarm-execute/SKILL.md` "Next Step — copy-paste to continue" — handoff pattern this skill emulates
