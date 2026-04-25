# Triage — Classes, Heuristics, Report Format

Reference for Step 3 of `/codex-adversary`: classify Codex free-text output into auto-fix / needs-confirmation / discuss / filtered classes, render triaged report.

## Triage classes

| Class | Definition | Action |
|---|---|---|
| **Auto-fix** | Clear root cause, small edit, obviously correct, no architectural/security implications, fits existing patterns, covered by tests | Apply via `Edit` / `Write`. Run `task verify`. Report done. |
| **Needs confirmation** | Correct observation but fix has design implications, touches security/auth, adds/removes dependency, changes public API, affects large surface | Present finding + proposed fix + tradeoff. Wait for user via `AskUserQuestion` or free-text. |
| **Discuss** | Codex raises real design question, no single right answer (e.g., "this approach assumes X — is that intentional?") | Present question verbatim with Claude take. User decides. No auto-answer. |
| **Filtered — trivia** | Subjective style, nit-pick formatting (`cargo fmt` handles it), comment wording | Drop. Mention count only. |
| **Filtered — stated convention** | Codex critiques thing explicitly fixed by `AGENTS.md` / `CLAUDE.md` (e.g., "consider Nix instead of OCI", "consider async-std instead of Tokio", "add Co-Authored-By") | Drop. Mention count only. |
| **Filtered — false positive** | Codex flags from stale/partial context; verify against current code before counting | Drop. If unsure, promote to *needs confirmation*. |

## Heuristics for "safe to auto-fix"

- Fix ≤10 lines, ≤3 files
- Fix entirely inside one subsystem (no cross-crate changes)
- No security/auth/secrets/crypto code touched
- No `pub` API surface changed
- No new dependencies, no version bumps
- No changes to `.claude/`, `CLAUDE.md`, `AGENTS.md`, quality gates
- Tests still pass (verify after)

If doubt, promote to **needs confirmation**. Better ask once than apply wrong "obvious" fix.

## Report format

Produce single triaged report:

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

Then, no further prompting, apply **Auto-fix** items. If any auto-fix has non-trivial implications during implementation (unexpected compile errors, cross-cutting changes), abort and reclassify as **needs confirmation**.