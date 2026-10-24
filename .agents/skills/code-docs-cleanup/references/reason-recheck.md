# Reason re-check

You loaded this file at step 9 of the cleanup: you shortened one or more guards
and need to show that a cold agent can still recover why each guarded line is
written the way it is, and that changing it breaks something.

Contents: [Why a fresh session](#why-a-fresh-session) ·
[What to probe](#what-to-probe) · [The session](#the-session) ·
[Turn 1: the unled question](#turn-1-the-unled-question) ·
[Turn 2: the led question](#turn-2-the-led-question) · [Scoring](#scoring) ·
[Pass and what to do otherwise](#pass-and-what-to-do-otherwise) ·
[Mistakes that void the re-check](#mistakes-that-void-the-re-check)

## Why a fresh session

- Refining existing code is the task most sensitive to comments. An agent that
  just rewrote a guard cannot un-know the version it deleted.
- A model warned that a comment may be wrong still discounts only a small part
  of its effect. The editing agent's own sense that the meaning survived is not
  evidence.
- A question that names the breaking edit hands over the answer. Leading
  wording measurably degrades code reasoning, which is why the unled question
  always comes first (CLN-06). In a blind eval, the led question got "keep" in
  all 272 sessions, including those whose guard comment was deleted and whose
  history was redacted. 86% of the sessions that failed the unled question then
  stated the right reason once led. A led answer alone proves nothing.

## What to probe

One probe per guard you shortened, reworded or moved. Skip guards you left
byte-identical, blocks with no guard clause (contract, narration, deleted
IDs), and a doc copy of a guard deleted while its at-line copy stays
byte-identical: deleting a duplicate is not a shortening. Take each probe's
inputs from the worklist written before the edit:

| Input | Source |
|---|---|
| `FILE`, `LINE` | The guarded code line as it sits in the edited tree |
| `ANCHOR` | That code line and one or two lines around it, copied from the edited file. Code only, not the comment |
| `EDIT` | The breaking edit recorded for this guard, phrased as a concrete change, such as "rename `_guard` to `_`" |
| `CONSEQUENCE` | The consequence recorded for this guard. It is the scoring key, never part of a prompt |

A guard with several breaking edits gets one led turn per edit, all in the same
session after the one unled turn.

A guard with no single implementing line, such as a file-level rule or a ban on
adding something (RTE-04), has no code line to anchor on. Set `FILE`, `LINE`
and `ANCHOR` to the test or check that asserts it. If none exists, skip the
probe and list the guard in the report for a human.

## The session

Run it against an exported copy of the edited tree: in touched-files mode
before you commit; in a sweep once, after the last file's commit, over every
guard the sweep shortened. Fix a failed probe in a new commit. It must be:

- **exported, with no git history:** `PROBE_DIR=$(mktemp -d); TREE=$(git stash
  create); git archive "${TREE:-HEAD}" | tar -x -C "$PROBE_DIR"`, then run the
  session with `$PROBE_DIR` as its working directory. `git stash create`
  captures uncommitted cleanup edits without touching the working tree, and
  falls back to `HEAD` once a sweep has committed the file. The export carries
  no `.git`, so the probe can read neither the commit message, which carries
  the guard's reason (step 10), nor the old comment through git history;
- **fresh:** no shared history with the editing session;
- **read-only:** Read, Grep, Glob and read-only shell (`rg`, `grep`, `ls`,
  `find`, `sed -n`, `head`, `tail`, `wc`). Edit, Write, web access and
  subagents are denied;
- **bounded:** a spend cap per probe (2 USD in the example);
- **ordinary:** it loads the project's own rules and instructions, as the next
  agent would, and runs on the model tier that makes routine edits, not the
  strongest one.

The Claude Code form, as of Claude Code 2.1 (2026-09). `P_UNLED` and `P_LED`
hold the prompts below with the placeholders filled:

```
PROBE_DIR=$(mktemp -d)
TREE=$(git stash create)
git archive "${TREE:-HEAD}" | tar -x -C "$PROBE_DIR"
TOOLS=(Read Grep Glob 'Bash(rg:*)' 'Bash(grep:*)' 'Bash(ls:*)' 'Bash(find:*)' 'Bash(sed -n:*)' 'Bash(head:*)' 'Bash(tail:*)' 'Bash(wc:*)')
DENY=(Edit Write NotebookEdit WebFetch WebSearch Agent Task)
OUT=$(cd "$PROBE_DIR" && claude -p --setting-sources project,local --model sonnet --allowedTools "${TOOLS[@]}" --disallowedTools "${DENY[@]}" --max-budget-usd 2 --output-format json "$P_UNLED" < /dev/null)
SID=$(printf '%s' "$OUT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["session_id"])')
printf '%s' "$OUT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"])'
(cd "$PROBE_DIR" && claude -p --resume "$SID" --setting-sources project,local --model sonnet --allowedTools "${TOOLS[@]}" --disallowedTools "${DENY[@]}" --max-budget-usd 2 --output-format json "$P_LED" < /dev/null) | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"])'
```

The arrays work in bash and zsh. On another client, reproduce the four
properties above with a fresh read-only subagent that receives only the prompt.

## Turn 1: the unled question

Asked first and scored. It must not suggest that anything would break.

```
Look at this code in `{FILE}`, near line {LINE}:

{ANCHOR}

Why is it written this way? Could it be written differently without changing
anything that matters? If something would break, say what, and where you found
that.

Answer in exactly this shape:
REASON: <one to three sentences>
CONSTRAINT: <none, or what breaks if it changes>
EVIDENCE: <file:line of a comment, doc, test, rule or record you relied on; or 'inferred from code'; or 'not found'>
```

## Turn 2: the led question

Asked after turn 1, in the same session, for each recorded breaking edit. It is
a consistency gate, never the score.

```
Same code. A colleague proposes this change: {EDIT}
Is that change safe to make?

Answer in exactly this shape:
VERDICT: keep | may-change
REASON: <one to three sentences>
EVIDENCE: <where you found the reason: file:line of a comment, doc, test, rule or record; or 'inferred from code'; or 'not found'>
```

## Scoring

Score the turn-1 answer against `CONSEQUENCE`, one of four levels. The example
column uses one guard: a lock guard bound to a named local.

| Level | The answer | Example |
|---|---|---|
| Mechanism plus consequence | Names what the line does to prevent the failure and the specific failure that follows | "The guard is bound to a named local because a value bound to `_` drops at once; renaming it releases the lock at that line, so two concurrent renders interleave instead of serialising." |
| Mechanism | Names what the line does, but not what breaks | "This holds the lock for the whole body through the named binding; renaming it to `_` would drop it early." |
| Generic caution | Senses importance without a mechanism | "This looks like an important synchronisation detail; I would check before renaming it." |
| Wrong | Calls the edit safe, or invents a reason that is not there | "Just an unused-variable convention; renaming to `_` is fine." |

- The top level needs the specific consequence. "This could cause a race" for a
  guard whose consequence is two renders interleaving is Mechanism, not the
  top level.
- The top level needs every consequence recorded for that edit. A guard that
  names several breaking edits scores the top level only when the unled answer
  names every edit's consequence; naming only some of them is Mechanism.
- Read `EVIDENCE`. An answer that got its reason from the base version, such as
  `git show HEAD:FILE` in touched-files mode or the sweep commit's parent, read
  the text you removed. Score it no higher than Generic caution.
- Turn 2 is a gate. A `may-change` verdict after a turn-1 answer at Mechanism or
  above means the answer does not hold up. Score the probe Generic caution.
- A `keep` verdict after a Generic or Wrong turn-1 answer raises nothing. The
  led question named the edit.

## Pass and what to do otherwise

| Turn-1 level, after the turn-2 gate | Meaning | Do |
|---|---|---|
| Mechanism plus consequence, and `keep` | The reason survives | Pass. Record the level in the commit body, or in a sweep the final report |
| Mechanism | The consequence clause was lost | Rewrite the guard to state the consequence at its line (decision list, section 2), then probe again |
| Generic caution | The mechanism was lost | Restore the guard's clauses from the base, rewrite with less cut, then probe again |
| Wrong | A cold agent would make the breaking edit | Restore the original block, keep it whole, and list it in the report |

- Before rewriting a guard that scored below the top, probe the base version
  once, in a scratch worktree at the base commit. If the base scores no higher,
  the original never carried the consequence either: keep your rewrite, and
  list the guard in the report for a human.
- List every probe that did not pass on the first run in the report, with both
  answers, for a human to read.
- Cost: two to four turns per shortened guard, each under the spend cap. In a
  sweep, run every shortened guard; that is the price of cutting legacy guards.

## Mistakes that void the re-check

| Mistake | Why it voids the result |
|---|---|
| Asking the led question first, or only the led question | The edit is named, so a model can agree by matching words |
| Probing from the editing session, or a subagent that saw the diff | It already knows the deleted text |
| Putting the original comment, the diff or `CONSEQUENCE` in a prompt | Same leak |
| Probing the comment text instead of the code line | A cold agent meets the code first, and the comment is what is under test |
| Granting Edit or Write | The probe may "fix" the code it is asked about |
| Counting a `keep` verdict alone as a pass | `keep` is the cautious default and needs no reason |
