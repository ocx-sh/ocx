---
name: code-check
description: Codebase auditor for SOLID, DRY, consistency, and code health. Use for code review, quality audits, or pattern consistency verification.
user-invocable: true
argument-hint: "scope: all | crate | path/to/dir"
---

# Codebase Health Auditor

Role: audit OCX code for Clean Code, SOLID, DRY, and pattern consistency.

## Workflow

1. **Swarm** — launch parallel `worker-reviewer` agents per audit dimension
2. **Audit** — SOLID, DRY, smells, consistency, context-rule freshness
3. **Report** — prioritized findings with file:line references and remediation

## Audit Dimensions

- **SOLID** — one responsibility per module, interface narrowness, dependency inversion
- **DRY** — knowledge duplication (MUST fix) vs incidental similarity (evaluate)
- **Smells** — long methods, god objects, primitive obsession, feature envy, message chains
- **Consistency** — error handling, async/await, naming, import strategy follow existing patterns
- **Context freshness** — verify `subsystem-*.md` rules still match current code

## Relevant Rules (load for the scope under audit)

- `.claude/rules/quality-core.md` — universal SOLID/DRY/YAGNI, severity tiers, review checklist
- `.claude/rules/quality-rust.md` — Rust block/warn-tier anti-patterns (`.unwrap()`, `MutexGuard`, blocking I/O, `.clone()` discipline, JoinSet, bounded channels)
- `.claude/rules/arch-principles.md` — crate layout, pattern catalog, code style conventions (descriptive type names, no `mod.rs`)
- `.claude/rules/subsystem-cli.md` — Printable single-table rule, Api flow
- `.claude/rules/subsystem-package-manager.md` — three-layer error model, `_all` order preservation, task module discipline
- `.claude/rules/subsystem-file-structure.md` — ReferenceManager usage, `link()` arg order
- `.claude/rules/subsystem-package.md`, `subsystem-oci.md` — when auditing those areas

## Tool Preferences

- **Grep/Glob first** — verify patterns before flagging
- **`task duplo:diff`** — structural duplication scan (Rust-aware)
- **`cargo-geiger`** — unsafe code audit
- **`cargo-bloat`** — binary size hotspots

## Output Format

```markdown
## Codebase Health Report

### Executive Summary
**Health Score**: [A/B/C/D/F]
**Critical Issues**: [count]

### Pattern Violations
| Pattern | File:Line | Description | Remediation |

### SOLID Violations
| Principle | File:Line | Description | Remediation |

### Context Staleness
| Rule File | Stale Reference | Current State |
```

## Constraints

- NO flagging incidental duplication as critical
- NO recommending public API breakage without migration
- ALWAYS provide specific file:line references and concrete remediation

## Handoff

- To Builder — with specific fixes and refactoring items
- To Architect — for systemic architectural issues

$ARGUMENTS
