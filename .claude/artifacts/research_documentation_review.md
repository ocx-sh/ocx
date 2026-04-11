# Research: Documentation Review Best Practices

> _Historical note: skill paths in this document predate the 2026-04 flatten. References to `.claude/skills/personas/...` map to `.claude/skills/...` post-flatten._

## Metadata

**Date:** 2026-03-22
**Purpose:** Inform `worker-doc-reviewer` and `worker-doc-writer` agent configurations
**Sources:** Tom Johnson API Doc Checklist, Diátaxis Framework, Red Hat Peer Review Guide, Rust API Guidelines, Write the Docs

## Key Findings

### Diátaxis Framework (adopted)

Four documentation types with strict separation:
- **Tutorial** — learning-oriented, for newcomers (getting-started.md)
- **How-to** — task-oriented, for practitioners (parts of user-guide.md)
- **Reference** — information-oriented, for lookup (reference/*.md)
- **Explanation** — understanding-oriented, for depth (user-guide.md)

Mixing types makes all content worse. Adopted as review checklist item.

### Documentation Trigger Matrix (key innovation)

Maps source code change patterns to required documentation updates. Eliminates the failure mode of "forgot to think about whether this needed docs." Embedded directly in `worker-doc-reviewer` agent.

### Anti-Patterns Identified

| Anti-pattern | Correct approach |
|---|---|
| Documenting from memory | Always read source first |
| Mixing reference and tutorial | Strict Diátaxis separation |
| Inline `[text](url)` links | Reference-style at file bottom |
| Marketing language | Let examples speak |
| Missing defaults in reference | Every flag/env var documents default |
| Stale code examples | Verify against binary |

### Trend: LLM Diff Audit in CI

GitHub Actions that auto-flag PRs as "documentation-relevant" by sending the diff to an LLM. The `worker-doc-reviewer` agent is this pattern at agent scale.

## Artifacts Created

- `.claude/agents/worker-doc-reviewer.md` — read-only documentation consistency reviewer
- `.claude/agents/worker-doc-writer.md` — documentation writer with OCX conventions
- Updated `.claude/skills/personas/swarm-review/SKILL.md` — added Documentation Consistency perspective
- Updated `.claude/rules/swarm-workers.md` — added both new worker types and focus modes
