---
name: code-check
description: Codebase auditor for SOLID, DRY, consistency, and code health. Use for code review, quality audits, or pattern consistency verification.
user-invocable: true
argument-hint: "scope: all | crate | path/to/dir"
---

# Codebase Health Auditor

Regular codebase review for Clean Code, SOLID, DRY principles and consistency in the OCX project.

## Audit Workflow

1. **Swarm** — Launch parallel worker-reviewer agents for each audit dimension
2. **SOLID** — Audit for principle violations
3. **DRY** — Detect knowledge duplication
4. **Smells** — Identify code smells
5. **Consistency** — Check OCX pattern consistency
6. **Context** — Verify subsystem context rules match current code
7. **Report** — Generate prioritized findings with remediation

## OCX Quality Checklist

### Error Model Compliance
- [ ] Package-specific errors use `PackageErrorKind` (not `crate::Error` directly)
- [ ] Single-item methods return `Result<T, PackageErrorKind>`
- [ ] `_all` methods return `Result<T, package_manager::error::Error>`
- [ ] `_all` methods preserve input order

### Symlink Safety
- [ ] Install symlinks use `ReferenceManager` (not raw `symlink::update/create`)
- [ ] `link()` arg order is `(forward_path, content_path)` — not reversed
- [ ] `unlink()` called for cleanup on error paths

### API Contract
- [ ] Each `Printable::print_plain()` produces exactly one `print_table()` call
- [ ] Headers are static `&str` (no `format!()`)
- [ ] Status values are enums with `Display` + `Serialize` (not raw strings)
- [ ] Report data built from task results (not echoed CLI args)

### Rust Quality (per rust-quality.md)
- [ ] No `.unwrap()` in library code
- [ ] No `MutexGuard` held across `.await`
- [ ] No blocking I/O in async context (`std::fs::*`, `std::thread::sleep`)
- [ ] `.clone()` is intentional (not borrow-checker workaround)
- [ ] `impl Trait` preferred over `Box<dyn Trait>` where possible
- [ ] JoinSet tasks preserve input order in `_all` methods
- [ ] Error types use `From` for `?` conversion
- [ ] Bounded channels (no `unbounded_channel` without justification)

### Pattern Consistency (per rust-quality.md §7)
- [ ] New code follows established OCX patterns (error model, progress, symlinks, CLI flow)
- [ ] Existing utilities were grepped before inventing new ones
- [ ] Similar patterns reused, not reinvented

### Reusability (per rust-quality.md §8)
- [ ] Generic logic in `ocx_lib`, command-specific in `ocx_cli`
- [ ] Cross-cutting concerns (progress, retry, rate-limiting) in library layer
- [ ] A second caller could import this function (not need to copy-paste)

### General
- [ ] No hardcoded secrets or credentials
- [ ] `cargo clippy` passes without warnings
- [ ] `cargo fmt` applied (120 char max width)
- [ ] No TODO/FIXME without associated issue
- [ ] Tests cover new code paths
- [ ] `duplo` run for structural duplication (interpret Rust-aware)

## Context Freshness Check

When auditing, also verify subsystem context rules match current reality:
- Are all public types mentioned in `subsystem-*.md` still present?
- Are error variants current?
- Are module paths correct?
- Any new modules not documented?

## Audit Dimensions

### SOLID Principles
Per `rust-quality.md` "SOLID in Rust" table: SRP, OCP, LSP, ISP, DIP.

### DRY Violations
- Knowledge duplication (MUST fix): Same business logic in multiple places
- Incidental duplication (evaluate carefully): Similar code that may evolve differently

### Code Smells
Long methods, large types, feature envy, data clumps, primitive obsession, message chains.

### Consistency
Error handling patterns, async/await usage, naming conventions, import strategies.

### Complexity
Use `cargo-geiger` for unsafe code, `cargo-bloat` for binary size.

## Output Format

```markdown
## Codebase Health Report

### Executive Summary
**Health Score**: [A/B/C/D/F]
**Critical Issues**: [count]

### OCX Pattern Violations
| Pattern | File:Line | Description | Remediation |

### SOLID Violations
| Principle | File:Line | Description | Remediation |

### Context Staleness
| Rule File | Stale Reference | Current State |
```

## Constraints

- NO flagging incidental duplication as critical
- NO recommending changes that break public APIs without migration
- ALWAYS provide specific file:line references
- ALWAYS suggest concrete remediation steps

## Handoff

- To Builder: With specific fixes and refactoring items
- To Architect: For systemic architectural issues

$ARGUMENTS
