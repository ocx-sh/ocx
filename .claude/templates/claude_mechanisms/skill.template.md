---
name: [skill-name]
description: [Clear description of what this skill does and when Claude should use it. Include trigger phrases like "Use when..." to help with skill invocation.]
allowed-tools: Read, Glob, Grep
---

<!--
Skills = model-invoked workflows. Discovered via description match.

Required fields:
  - name: lowercase, hyphens only, max 64 chars, must match directory name
  - description: max 1024 chars, CRITICAL for auto-discovery

Optional fields:
  - allowed-tools: Comma-separated list to restrict tool access

Description best practices:
  GOOD: "API design skill. Use when designing REST APIs, GraphQL schemas, or gRPC services."
  BAD:  "Helps with APIs"

Supporting files: drop sibling .md next to SKILL.md for progressive
disclosure (e.g. commit_reference.md). No subdirs — Claude Code only
finds skills at .claude/skills/<name>/SKILL.md exact.

Location: .claude/skills/[skill-name]/SKILL.md
-->

# [Skill Name]

## Overview

[Skill purpose + scope, brief]

## Workflows

- [ ] **Step 1**: [Description]
- [ ] **Step 2**: [Description]
- [ ] **Step 3**: [Description]

## Feedback Loops

1. [Action]
2. [Validation]
3. If [condition], [correction]
4. Repeat til [success criteria]

## Reference Implementation

```[language]
// Example code demonstrating the pattern
```

## Best Practices

- [Practice 1]
- [Practice 2]
- [Practice 3]

## Anti-Patterns

- [What to avoid 1]
- [What to avoid 2]

## Resources

- `[Sibling Reference](./[skill-name]_reference.md)` — optional progressive-disclosure file beside SKILL.md
- `[External Link](https://example.com)`