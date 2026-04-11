---
name: swarm-review
description: Adversarial multi-perspective code reviewer. Use for thorough code review with security, performance, architecture, and SOTA analysis.
user-invocable: true
argument-hint: "branch-name-or-pr-number"
---

# Adversarial Reviewer

Multi-perspective code review with root cause analysis and OCX-specific pattern verification.

## Review Workflow

1. **Gather** — Get diff and commit history for the branch. Compute changed file list via `git diff main...HEAD --name-only`.
2. **Analyze** — Launch parallel worker-reviewer agents for each perspective, scoped to changed files only.
3. **Classify** — Each reviewer classifies findings as **actionable** (can fix without human input) or **deferred** (needs human decision).
4. **Interrogate** — Apply adversarial questioning
5. **Root Cause** — Investigate systemic issues with Five Whys
6. **Verdict** — Approve or request changes

## Review Perspectives

Launch parallel workers for:

### OCX Pattern Compliance
- Error model: `PackageErrorKind` used correctly, three layers maintained
- Symlink safety: `ReferenceManager` used (not raw symlinks)
- API contract: single table, static headers, enum statuses, actual results
- Command pattern: args → manager → report (not echoing CLI args)

### Security
Per `.claude/rules/quality-security.md`: OWASP Top 10, CWE references, auth flow, input validation, symlink traversal, archive extraction safety.

### Performance
Per `.claude/rules/quality-core.md`: N+1 patterns, blocking I/O in async paths, memory allocations, pagination, caching opportunities.

### Architecture
SOLID principles, subsystem boundary respect, dependency direction.

### Test Coverage
New code has tests. Bug fixes have regression tests. Edge cases covered.

### Rust Quality
Per `.claude/rules/quality-rust.md`: Block/Warn/Suggest tier items, async correctness (JoinSet order, cancel safety, bounded channels), SOLID compliance in Rust, code duplication detection.

### Pattern Consistency & Reusability
Per `.claude/rules/arch-principles.md` (Code Style Conventions, Design Principles), `.claude/rules/subsystem-*.md` (per-subsystem patterns), and `.claude/rules/quality-core.md` (Reusability Assessment):
- Does new code follow established OCX patterns or reinvent them?
- Is generic logic in the right layer (`ocx_lib` vs `ocx_cli`)?
- Could a second command reuse this code, or would it need to copy-paste?
- Are cross-cutting concerns (progress, retry, rate-limiting) in the library?

### Documentation Consistency
Launch `worker-doc-reviewer` to check code-documentation drift:
- Cross-reference changed files against the documentation trigger matrix
- New CLI commands/flags → `reference/command-line.md`
- New env vars → `reference/environment.md`
- New schema fields → `reference/metadata.md`
- Changed behavior → `user-guide.md` accuracy
- New platforms → `installation.md` + `user-guide.md`
- Breaking changes → `changelog.md`

### CLI UX Quality
Use `worker-reviewer` (focus: quality) to evaluate CLI user experience against package manager UX standards:
- Error messages: actionable (what happened + what to do next), copy-pasteable recovery commands, stderr-only
- Progress & feedback: indicator within 100ms of any network/disk op, X-of-Y for batch ops, streaming for blobs
- Output design: stdout-for-data / stderr-for-messages split, `--format json` stability, no ANSI leaks into JSON
- Command structure: `--help` shows env var overrides, flag naming matches conventions, `--` separator documented with examples
- Dependency UX: `deps --why` traces, `install` reports what was resolved, re-install distinguishes fresh vs cached
- Machine/CI UX: meaningful exit codes, `CI=true` suppresses interactive output, `--offline` fails fast with clear error
Reference: `.claude/artifacts/research_cli_ux.md` for the full checklist and sources (clig.dev, 12 Factor CLI Apps, Evil Martians progress patterns, miette diagnostics).

### Technical Soundness & SOTA
Use `worker-researcher` to compare the implementation against industry state of the art:
- How do leading tools (Cargo, npm, pip/uv, Go modules, Helm) solve the same problem?
- Is the algorithm choice current (e.g., PubGrub, SAT solvers, topological BFS)?
- Does the data format align with established standards (semver ranges, OCI artifacts)?
- Are there known pitfalls in the domain that the implementation doesn't address (lock files, supply chain security, dependency confusion)?
- What emerging patterns or recent publications are relevant?

## Adversarial Questions

- "What if this assumption is wrong?"
- "Under what conditions would this fail?"
- "What edge cases weren't considered?"
- "What happens when [X] fails?"
- "How does this behave under load?"

## Root Cause Analysis

```markdown
**Issue**: [Describe the problem]
**Why 1**: [First-level cause]
**Why 2**: [Deeper cause]
...
**Systemic Fix**: [What prevents recurrence]
```

## Verdict

**Approve** when: All Critical/High resolved, tests pass, matches conventions.
**Request Changes** when: Security vulnerabilities, breaking changes without migration, missing tests, architectural violations.

## Output Format

```markdown
## Code Review: [Branch/PR]

### Summary
**Verdict**: Approved | Needs Work | Request Changes

### OCX Pattern Violations
- [ ] [File:Line] [Violation] - [Remediation]

### Security Issues
### Performance Issues
### Architecture Issues
### Test Coverage Gaps
### CLI UX Issues
### Technical Soundness & SOTA
```

## Constraints

- NO approving with unresolved Critical/High issues
- NO nitpicking style when using rustfmt
- ALWAYS reference specific files and lines
- ALWAYS suggest alternatives, not just problems

## Handoff

- To Builder: With specific remediation tasks
- To Doc Writer: With gap report from documentation reviewer
- To Architect: For architectural concerns requiring ADR

$ARGUMENTS
