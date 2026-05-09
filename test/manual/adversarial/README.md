# Adversarial test review

A two-agent pattern for catching weak tests before they merge: one writes
the test (the **implementer**), the other audits it (the **loophole
searcher**). Run them in parallel via Claude Code's `Agent` tool. The
implementer never blocks waiting for human review — the searcher's report
goes to `reports/<topic>.md` for asynchronous reading.

## When to use

- Writing a test for a non-trivial concurrency / I/O boundary
  (offline mode, singleflight dedup, atomic temp moves, codesign edge
  cases).
- Adding a regression test where the bug class is subtle (the prior
  failure mode is easy to mis-write a test for).
- Reviewing an external contributor's PR that adds a test and you don't
  fully trust it covers the intent.

## When not to use

- Pure unit tests with single happy/sad paths.
- Style or linter fixes — the loophole framing wastes both agent runs.
- Time-boxed fixes — the loop has overhead; for trivial work just write
  the test inline.

## Process

1. **Stub the test.** Write the failing test in `test/tests/test_*.py`
   (or a `.sh` under `test/scenarios/<topic>/` for shell-driven flows).
   It must compile and run; "fails for the right reason" is the bar.
2. **Spawn both agents in a single message** so they run concurrently:
   - `test-implementer` — given the bug report and the stub, finishes
     the assertions and produces the final test diff. Tools: `Read`,
     `Edit`, `Write`, `Bash`.
   - `loophole-searcher` — given the same bug report and the stub, and
     **only the stub** (no access to the implementer's draft), audits
     the test for missed assertions, race conditions, weak fixtures,
     and suggests at least three concrete weaknesses. Tools: `Read`,
     `Grep`. Writes a markdown report to
     `test/manual/adversarial/reports/<topic>.md`.
3. **Read both outputs.** The implementer hands back a diff; the
   searcher hands back a report. Reconcile by hand — do not let the
   implementer auto-apply searcher suggestions in the same session.
4. **Pick which findings to action.** Open a follow-up TODO for any
   deferred. Commit the implementer's diff as `test:` or `fix:` per
   the bug class.

## Prompt templates

### Implementer

```
You are extending a stub test into the final form. Bug report below; the
existing stub is at <path>. Add the assertions needed to prove the
behaviour. Constraints:
- No new fixtures unless absolutely required.
- The diff must be small enough to read in one screen.
- Use the Scenario harness from test/src/scenarios/ where it fits.
Bug report:
<…>
Stub:
<…>
```

### Loophole searcher

```
You are auditing the stub test below for weaknesses. Identify at least
three concrete loopholes — situations the test would pass while the
underlying bug remains. For each, name the loophole, explain why it
slips through, and propose the smallest assertion that would close it.
Do NOT implement the test; produce a markdown report only. Constraints:
- Read code; do not edit.
- Look at concurrency, partial failures, fixture isolation, error-class
  drift (e.g., the test passes against a generic Failure when the bug
  needed a specific OfflineBlocked exit code).
- Cite exact file paths + line numbers.
Stub:
<…>
Write the report to test/manual/adversarial/reports/<topic>.md.
```

## Why no committed agent definitions?

Per project preference: AI must not halt on manual review. Adversarial
agents are spawned on demand; there is no `.claude/agents/test-*.md`
that auto-loads on every PR. The cost of a generic agent loop on every
test edit is higher than the value; targeted invocations are cheap.
