# Conventional Commits v1.0.0 — Cheat Sheet

Reference for the `/commit` skill. Full spec: https://www.conventionalcommits.org/en/v1.0.0/

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
| `perf` | Performance improvement, behaviour unchanged | Yes |
| `refactor` | Structure change, behaviour unchanged | Yes |
| `docs` | Documentation only | Yes |
| `test` | Tests only | Yes |
| `build` | Build system, dependencies, Cargo.toml | Yes |
| `ci` | CI configuration (workflows, actions) | Yes |
| `chore` | **AI/tooling files, `.claude/`, CLAUDE.md, skills, rules, hooks, taskfiles** | **No** |
| `style` | Formatting only (prefer not to use — `cargo fmt` handles it) | No |

OCX convention: use `chore:` for anything under `.claude/` or other AI-config files so they stay out of the user-facing changelog.

## Scope

Optional noun describing the area touched. Examples from this repo:

- `feat(cli): add --remote flag to index catalog`
- `fix(oci): flush AsyncWrite before closing blob file`
- `refactor(package-manager): flatten PackageErrorKind variants`
- `chore(claude): tighten builder skill description`

Only add a scope when it genuinely narrows the change. Skip it for cross-cutting work.

## Description

- Imperative mood: `add`, `fix`, `remove` — not `added`, `fixes`, `removing`.
- Lowercase first letter.
- No trailing period.
- ≤72 characters for the full subject line.

Bad: `Added a new feature to the installer.`
Good: `feat(installer): auto-detect existing candidates`

## Body (optional)

Explain **why**, not what — the diff already shows what. Only include a body when the reason is non-obvious (hidden constraint, subtle invariant, workaround for a specific bug, context a future reader would miss).

Wrap at ~80 characters. Plain prose, no markdown bullet soup unless genuinely listing discrete items.

## Footers (optional)

Format: `Token: value` or `Token #reference`. Tokens use hyphens, not spaces.

Common footers:

- `BREAKING CHANGE: <description>` — mandatory for breaking changes (even if `!` is already used in the subject). This is the only footer where spaces are allowed in the token.
- `Refs: #123` — reference an issue without closing it.
- `Closes: #123` — close an issue when the commit lands on the default branch.

**Never use `Co-Authored-By`** in this repo.

## Breaking Changes

Two signals, used together:

1. `!` before the colon: `feat(api)!: remove deprecated install --force flag`
2. Footer: `BREAKING CHANGE: --force has been replaced by --select; see migration notes.`

Both should appear. The `!` makes scanning fast; the footer gives the detail.

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
- **Explaining what instead of why** — the diff shows what; the body is for the why
- **Bullet-pointed body for a single-line change** — prose is fine; bullets are noise
- **Scope that duplicates the type** — `feat(feature):` adds nothing
- **Multiple concerns in one commit** — split it; one concern per commit
