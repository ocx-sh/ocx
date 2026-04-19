# Triage — Classes, Heuristics, Report Format

Reference for Step 3 of `/codex-adversary`: how to classify Codex's free-text output into auto-fix / needs-confirmation / discuss / filtered classes, and how to render the triaged report.

## Triage classes

| Class | Definition | Action |
|---|---|---|
| **Auto-fix** | Clear root cause, small edit, obviously correct, no architectural or security implications, fits existing patterns, covered by test suite | Apply directly via `Edit` / `Write`. Run `task verify`. Report what was done. |
| **Needs confirmation** | Correct observation but the fix has design implications, touches security/auth, introduces or removes a dependency, changes a public API, affects a large surface | Present the finding + proposed fix + tradeoff. Wait for user decision via `AskUserQuestion` or free-text. |
| **Discuss** | Codex raises a real design question with no single right answer (e.g., "this approach assumes X — is that intentional?") | Present the question verbatim with Claude's take. Let the user decide. Do not auto-answer. |
| **Filtered — trivia** | Subjective style, nit-pick formatting (`cargo fmt` handles it), wording in comments | Drop. Mention count only. |
| **Filtered — stated convention** | Codex critiques something explicitly fixed by `AGENTS.md` / `CLAUDE.md` (e.g., "consider Nix instead of OCI", "consider async-std instead of Tokio", "add Co-Authored-By") | Drop. Mention count only. |
| **Filtered — false positive** | Codex flags something based on stale or partial context; verify against current code before counting it | Drop. If you are not sure it is false, promote to *needs confirmation* instead. |

## Heuristics for "safe to auto-fix"

- The fix is ≤10 lines and affects ≤3 files
- The fix is entirely inside one subsystem (no cross-crate changes)
- No security/auth/secrets/crypto code touched
- No `pub` API surface changed
- No new dependencies, no version bumps
- No changes to `.claude/`, `CLAUDE.md`, `AGENTS.md`, or quality gates
- Tests still pass (verify after)

If in doubt, promote to **needs confirmation**. Better to ask once than to apply a wrong "obvious" fix.

## Report format

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

Then, without further prompting, apply the **Auto-fix** items. If any auto-fix has non-trivial implications discovered during implementation (unexpected compile errors, cross-cutting changes), abort that item and reclassify it as **needs confirmation**.
