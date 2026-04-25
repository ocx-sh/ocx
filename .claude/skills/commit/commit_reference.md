# Conventional Commits v1.0.0 — Cheat Sheet

Reference for `/commit` skill. Full spec: https://www.conventionalcommits.org/en/v1.0.0/

## Structure

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

- Blank line between subject and body.
- Blank line between body and footers.

## Types (OCX usage)

| Type | Meaning | Changelog? |
|---|---|---|
| `feat` | New feature or capability | Yes (MINOR) |
| `fix` | Bug fix | Yes (PATCH) |
| `perf` | Perf improvement, behavior unchanged | Yes |
| `refactor` | Structure change, behavior unchanged | Yes |
| `docs` | Docs only | Yes |
| `test` | Tests only | Yes |
| `build` | Build system, deps, Cargo.toml | Yes |
| `ci` | CI config (workflows, actions) | Yes |
| `chore` | **AI/tooling files, `.claude/`, CLAUDE.md, skills, rules, hooks, taskfiles** | **No** |
| `style` | Formatting only (prefer skip — `cargo fmt` handles) | No |

OCX rule: `chore:` for anything under `.claude/` or AI-config files so stay out of user changelog.

## Scope

Optional noun for area touched. Examples from repo:

- `feat(cli): add --remote flag to index catalog`
- `fix(oci): flush AsyncWrite before closing blob file`
- `refactor(package-manager): flatten PackageErrorKind variants`
- `chore(claude): tighten builder skill description`

Add scope only when narrows change. Skip for cross-cutting work.

## Description

- Imperative mood: `add`, `fix`, `remove` — not `added`, `fixes`, `removing`.
- Lowercase first letter.
- No trailing period.
- ≤72 chars full subject line.

Bad: `Added a new feature to the installer.`
Good: `feat(installer): auto-detect existing candidates`

## Body (optional)

Explain **why**, not what — diff show what. Include body only when reason non-obvious (hidden constraint, subtle invariant, workaround for specific bug, context future reader miss).

Wrap ~80 chars. Plain prose, no markdown bullet soup unless listing discrete items.

## Footers (optional)

Format: `Token: value` or `Token #reference`. Tokens use hyphens, not spaces.

Common footers:

- `BREAKING CHANGE: <description>` — mandatory for breaking changes (even if `!` in subject). Only footer where spaces allowed in token.
- `Refs: #123` — reference issue without closing.
- `Closes: #123` — close issue when commit lands on default branch.

**Never use `Co-Authored-By`** in this repo.

## Breaking Changes

Two signals, used together:

1. `!` before colon: `feat(api)!: remove deprecated install --force flag`
2. Footer: `BREAKING CHANGE: --force has been replaced by --select; see migration notes.`

Both appear. `!` fast scan; footer give detail.

## Worked Examples

### Simple feature
```
feat(index): add --remote flag to catalog command
```

### Bug fix with context
```
fix(oci): flush AsyncWrite before closing blob file

tokio::fs::File returns Poll::Ready from poll_write before the
OS-level write actually completes. Without an explicit flush the
file can be closed mid-write, producing truncated blobs that only
surface after a subsequent pull.
```

### Breaking change
```
refactor(cli)!: require --select for install tag resolution

BREAKING CHANGE: `ocx install <pkg>` without --select now errors
when multiple tags match. Previously it picked the lexicographic
maximum, which was surprising. Use `ocx install --select <pkg>:<tag>`
to restore the old behaviour against a specific tag.
```

### AI config change (chore, no changelog entry)
```
chore(claude): add /commit skill with PR-prompt memory
```

## Common Mistakes

- **Title case description** — `feat: Add foo` should be `feat: add foo`
- **Past tense** — `fix: fixed the bug` should be `fix: fix the bug`
- **Explain what not why** — diff show what; body for why
- **Bullet body for single-line change** — prose fine; bullets noise
- **Scope duplicate type** — `feat(feature):` add nothing
- **Multiple concerns one commit** — split; one concern per commit