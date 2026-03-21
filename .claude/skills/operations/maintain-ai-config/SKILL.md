---
name: maintain-ai-config
description: Maintain, audit, and update the AI configuration (.claude/ directory). Use when creating/editing skills, rules, agents, hooks, or when project patterns have evolved and AI knowledge needs refreshing. Also use periodically to sync AI artifacts with current codebase state. This skill itself should be refreshed when Claude Code releases new features.
user-invocable: true
argument-hint: "create | audit | refresh | review | research topic"
---

# AI Configuration Maintenance

Maintain the `.claude/` directory as a living knowledge base that stays current with both the project and AI tooling best practices.

**Read `.claude/rules/meta-ai-config.md` first** — it defines the conventions, budget constraints, and anti-patterns this skill enforces.

## Modes

### `create` — New Artifact

1. **Research** — Spawn two agents in parallel:
   - `worker-researcher`: Claude Code best practices for this artifact type (code.claude.com/docs) + domain best practices for the artifact's topic
   - `worker-explorer`: Existing `.claude/` patterns, conventions, cross-references

2. **Draft** — Follow `meta-ai-config.md` conventions:
   - Respect context budget (<200 lines rules, <500 lines skills)
   - Use `paths:` scoping for rules unless truly global
   - Skills: write description as "what + when to use" (max 1024 chars)
   - Use progressive disclosure — SKILL.md overview, reference files for detail
   - Add `disable-model-invocation: true` for action skills with side effects

3. **Integrate** — Update `skill-rules.json`, CLAUDE.md tables, meta-rule inventory

4. **Validate** — Run audit checks (see below)

### `audit` — Check All Artifacts

**Context budget audit:**
- CLAUDE.md under 200 lines?
- Each global rule under 200 lines?
- Count global rules — too many degrades performance
- Skill descriptions total within 2% context budget?

**Structural audit:**
- Every `skill-rules.json` path → existing file?
- Every SKILL.md has `name` + `description`?
- No `allowed-tools` in skill frontmatter?
- All persona skills have `user-invocable: true`?
- No orphan skills (on disk but missing from `skill-rules.json`)?
- Hook scripts are executable?

**Dead glob audit:**
- For each scoped rule, do `paths:` patterns match any existing files?
- After directory renames, glob patterns silently fail — verify with: `find . -path "pattern" | head -1`

**Cross-reference audit:**
- Rules referencing other rules → targets exist?
- Skills referencing rules → correct filenames?
- Agents referencing rules → still valid?

**Duplication audit:**
- Same instruction in CLAUDE.md AND a rule? (single source of truth)
- Same domain knowledge in a skill AND a rule? (skill for on-demand, rule for always-on)

### `refresh` — Sync AI Knowledge with Codebase

1. **Detect drift** — Spawn `worker-explorer` agents:
   - Public types in `subsystem-*.md` still exist in code?
   - New modules/crates lacking subsystem rules?
   - Error variants, trait names, method signatures still match?
   - CLI commands changed (new flags, subcommands)?
   - `deny.toml` / `.licenserc.toml` changed?

2. **Research updates** — Spawn `worker-researcher`:
   - Claude Code docs for new features (hooks, frontmatter, agents)
   - New best practices in Rust, async, testing, security
   - Check if `meta-ai-config.md` itself needs updating

3. **Update stale artifacts** — Read current code, update with accurate info, preserve structure

4. **Self-update** — Check if this skill (`maintain-ai-config`) or `meta-ai-config.md` are outdated based on research findings. Update them too.

5. **Validate** — Run `audit` mode

### `review` — AI Config Quality Review

Review recent changes to `.claude/` for quality:

1. **Context budget** — Does this change increase always-loaded context? Is it justified?
2. **Scoping** — Could this global rule be path-scoped instead?
3. **Progressive disclosure** — Is the SKILL.md body under 500 lines? Should details move to reference files?
4. **Description quality** — Is the skill description specific enough for auto-discovery?
5. **Anti-patterns** — Check against the 8 anti-patterns in `meta-ai-config.md`
6. **Consistency** — Does the change follow existing artifact conventions?
7. **Reusability** — Is a hook more appropriate than a rule? (deterministic + zero context cost)

### `research` — Deep-Dive a Topic

1. **Spawn researchers** — 2-3 `worker-researcher` agents in parallel:
   - Claude Code / AI tooling best practices
   - Specific domain (Rust patterns, OCI spec, cargo-deny, etc.)
   - Community patterns (how other projects handle this)

2. **Synthesize** → Actionable guidance
3. **Apply** → Update relevant artifacts

## Refresh Targets

| Artifact | What goes stale | Refresh trigger |
|----------|----------------|-----------------|
| `subsystem-*.md` rules | Types, paths, signatures, error variants | After refactors, new modules |
| `rust-quality.md` | OCX patterns table, async conventions | After new patterns, new tooling |
| Persona skills | Implementation patterns, fixtures, commands | After new commands, workflows |
| Agent definitions | Patterns, self-review checklists | After rust-quality.md changes |
| `skill-rules.json` | Keywords, file patterns | After new file types, terminology |
| `dependency-management` | License allowlist, tool versions | After deny.toml changes |
| `CLAUDE.md` | Build commands, env vars, layout | After new crates, env vars |
| `meta-ai-config.md` | Conventions, budget numbers, anti-patterns | After Claude Code releases |
| **This skill** | Modes, workflow, refresh targets | After Claude Code releases |

## Maintenance Schedule

| Frequency | Action |
|-----------|--------|
| Every feature branch | `audit` before merging AI config changes |
| Monthly | `refresh` to detect drift |
| On Claude Code update | `research "Claude Code new features"` then self-update |
| On new tool integration | `create` for tool's skill/rule, then `audit` |
| When something feels off | `review` recent changes |

## Constraints

- ALWAYS research online before creating AI artifacts
- ALWAYS spawn at least one `worker-researcher` for domain knowledge
- ALWAYS validate `skill-rules.json` after changes
- ALWAYS check context budget impact (is this adding always-loaded context?)
- NEVER remove artifacts without checking cross-references first
- NEVER edit `settings.json` hooks without testing the hook script
- Prefer hooks over rules for enforcement (deterministic + zero context cost)
- Commits use `chore:` prefix (per project convention)
