# Plan: `.claude/` Quality Rules Reorganization

## Context

Michael is a solo developer using Claude Code exclusively for the OCX project. After completing the `.claude/skills/` flatten cleanup (two commits now landed on `evelynn`), he noticed a deeper architectural issue with the language "skills" (`python`, `rust`, `typescript`, `bash`, `vite`):

> *"I don't think these are really skills. It's more like a quality check. Especially for the Python skill isn't this more like a rule that should be loaded depending on language? […] I rather would expect is that there is some kind of unified markdown, which points to language specific markdown files and also to non-language specific markdown files for quality checks. And this is more like a rule, like one root rule. And then like rules for architecture and for Python and for Rust and so forth."*

**Primary goal**: *"that the implementation of skills is kind of more or less independent of the project structure and that workflows and quality guidelines are project dependent and are just in reference markdown files which then will be read as previously stated, holding like the entry point from the skills relatively small and then again point to subsystem or language or context specific rulesets."*

**Secondary goal**: *"Maybe in future I want to share these quality files within different repositories."* — the new `quality-*.md` rule files should be project-independent so Michael can copy or symlink them into other repos later.

### Evidence from current state (verified by exploration)

1. **Language "skills" aren't skills by the decision tree**. `.claude/rules/meta-ai-config.md` says:
   > Must Claude know it every session? → Yes → Is it file/directory-specific? → Yes → `.claude/rules/` with `paths:` scoping
   >
   > Manual with side effects → Skill with `disable-model-invocation: true`
   > Auto-triggered by context → Skill with good description
   Language best practices are "yes/yes" — context Claude needs when a `*.py` is on screen, not a procedure to invoke on demand. That's a scoped rule, not a skill.

2. **`rust/SKILL.md` already admits it**. Line 10: *"Read `.claude/rules/rust-quality.md` for the comprehensive quality guide"*. The skill is a pointer to the rule — dead weight.

3. **Duplication of SOLID/DRY/YAGNI across 6 files**. `code-quality.md`, `rust-quality.md`, `python/SKILL.md`, `rust/SKILL.md`, `typescript/SKILL.md`, `bash/SKILL.md` — each re-explains the same cross-language principles in subtly different words. `python/SKILL.md` and `typescript/SKILL.md` even claim in their first paragraph that the principles live in `code-quality.md` — and then duplicate them anyway (~60 lines each).

4. **`rust-quality.md` leaked OCX-specific content** into a supposedly language rule: "OCX example" blocks (`ClientBuilder`, `BundleBuilder`, `ReferenceManager`, `IndexImpl`, `PackageManager`), a full "Project Pattern Consistency" table with 14 rows of OCX conventions (`Printable` trait, `to_relaxed_slug()`, `symlink::is_link()`, etc.), and an "OCX layer guide" table. 194 lines total, ~50 of them OCX-specific. Blocks the "shareable" goal.

5. **`code-quality.md` is mixed**. Lines 1-93 are genuinely cross-language and shareable. Lines 94-131 are OCX-specific quality gates (`task verify`, `cargo-nextest`, `hawkeye`, `ocx install shellcheck`, Docker registry, etc.) that are already documented in CLAUDE.md's "Build & Development Commands" section — duplication of a different kind.

6. **`vite/SKILL.md` is 732 lines** of Vite cookbook material — too large for progressive disclosure guideline (<500 lines per meta-ai-config.md) and mostly authoritative-docs-adjacent (users can read vite.dev).

7. **Subsystem rules already cover the OCX content** that would be stripped from `rust-quality.md`:
   - `subsystem-package-manager.md` line 13 covers three-layer error model (`Error → PackageError → PackageErrorKind`), line 85 covers `_all` batch methods returning `Result<T, Error>`
   - `subsystem-file-structure.md` line 98+ covers `ReferenceManager` public API
   - `subsystem-cli.md` line 60+ covers the `Printable` trait and `report_*()` pattern
   Nothing would be lost by deleting the OCX material from `rust-quality.md` — it already lives in the right place.

8. **Current suggestion-hook masking**. After the previous flatten commit, all 20 project skills are discoverable, but the language skills still surface as top-priority suggestions on keywords like "python", "rust", "typescript", "bash", "ci" — crowding the suggestion list and training Claude to reach for them when it shouldn't.

### Backup / review strategy (not commit strategy)

Per Michael's guidance: *"we will squash all commits on this branch anyway making one big AI update commit. So you can kind of feel free to organize it more like a backup strategy and review strategy rather than commits that land on main."*

This plan is organized into **phases** for reviewability, checkpointed via `task checkpoint` (amend-into-Checkpoint pattern). No bisect-friendly boundaries needed — the whole reorg lands as one squashed commit on `main` eventually. Phase boundaries exist so Michael can stop and review between them.

---

## Target Architecture

```
.claude/
├── rules/
│   ├── code-quality.md          # SHAREABLE root: SOLID/DRY/YAGNI, severity tiers,
│   │                            # universal anti-patterns, review checklist,
│   │                            # reusability, performance, Two Hats refactoring.
│   │                            # ~100 lines, no OCX content, no paths: frontmatter.
│   │
│   ├── quality-rust.md          # SHAREABLE, path-scoped to **/*.rs, **/Cargo.toml
│   │                            # (renamed from rust-quality.md; OCX content stripped)
│   ├── quality-python.md        # NEW, shareable, **/*.py, **/pyproject.toml
│   ├── quality-typescript.md    # NEW, shareable, **/*.ts, **/*.tsx, **/*.mts, **/*.cts, **/tsconfig*.json
│   ├── quality-bash.md          # NEW, shareable, **/*.sh, **/*.bash
│   ├── quality-vite.md          # NEW, shareable, **/vite.config.*, **/vitest.config.*, **/.vitepress/**
│   │
│   ├── subsystem-*.md           # UNCHANGED — OCX-specific Rust patterns live here
│   └── (other rules unchanged)
│
└── skills/
    ├── (12 legitimate personas/operations/procedures — unchanged)
    │
    └── ❌ DELETED:
        ├── python/        — content absorbed into quality-python.md
        ├── rust/          — pointer-skill, content lives in quality-rust.md + subsystem rules
        ├── typescript/    — content absorbed into quality-typescript.md
        ├── bash/          — content absorbed into quality-bash.md
        └── vite/          — 732 lines → ~100-line quality-vite.md + "read vite.dev for the rest"
```

**Skill count**: 20 → 15.

**Invariant** (to be enforced by new structural test): any file under `.claude/rules/quality-*.md` must not contain OCX-specific strings (`PackageErrorKind`, `ReferenceManager`, `ocx_lib`, `PackageManager`, `to_relaxed_slug`, `Printable` trait). Same for `code-quality.md` after the strip.

---

## Phase 1 — Draft the shareable root: `code-quality.md`

**Goal**: Strip OCX-specific content from `code-quality.md` so it becomes a 100% shareable root rule.

**Changes to `.claude/rules/code-quality.md`**:

1. **Keep as-is** (lines 1-93, ~92 lines):
   - Header paragraph (update reference list to include `quality-python.md`, `quality-typescript.md`, `quality-bash.md`, `quality-vite.md`)
   - Design Principles (SOLID table, DRY bullets, YAGNI bullets)
   - Anti-Pattern Severity (Block/Warn/Suggest table + universal anti-patterns)
   - Reusability Assessment
   - Code Review Checklist (All Languages)
   - Performance Checklist

2. **Delete**: lines 94-131 (Quality Gates + Prerequisites + Gates order). This content is OCX-specific and already duplicated in `CLAUDE.md` "Build & Development Commands" section.

3. **Delete**: lines 133-137 (Taskfile Conventions). This is OCX-specific design convention for taskfile authoring. Move to `CLAUDE.md` Build section as 3 bullets under a new "### Taskfile conventions" subheading. **If inlining bloats CLAUDE.md past 200 lines, create a new `.claude/rules/taskfile-conventions.md` scoped to `**/taskfile*.yml, **/taskfiles/**` instead.**

4. **Keep**: lines 139-147 (Refactoring Discipline — Two Hats Rule). This is universal.

5. **Add at the bottom**: a "See Also" section pointing to the language-specific leaves:
   ```markdown
   ## See Also — Language-Specific Quality Rules

   - `quality-rust.md` — Rust-specific quality (ownership, async/Tokio, error handling)
   - `quality-python.md` — Python-specific quality (type hints, async, tooling)
   - `quality-typescript.md` — TypeScript-specific quality (strict mode, ESM, narrowing)
   - `quality-bash.md` — Bash script safety
   - `quality-vite.md` — Vite build-tool opinions
   ```

**Target size**: ~110 lines.

**Frontmatter**: none (global rule, loaded every session).

---

## Phase 2 — Rename and strip `rust-quality.md` → `quality-rust.md`

**Goal**: Pure Rust language-quality reference, no OCX content.

### 2.1 Move via `git mv`

```sh
git mv .claude/rules/rust-quality.md .claude/rules/quality-rust.md
```

### 2.2 Update frontmatter

Change `paths:` from OCX-specific to generic:

```yaml
---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
  - "**/Cargo.lock"
---
```

### 2.3 Content strip

**Delete** the following OCX-specific content:

| Section | Lines (original) | Fate |
|---|---|---|
| "OCX example: `ClientBuilder`, `BundleBuilder`" | 16 | Delete (keep Builder Pattern description) |
| "OCX example: `TempAcquireResult`…" | 22 | Delete (keep RAII description) |
| "OCX example: `IndexImpl` trait…" | 25 | Delete (keep Strategy description) |
| "OCX convention: `_all` methods preserve input order…" | 106 | Delete (already in `subsystem-package-manager.md`) |
| Full "OCX Async Conventions" subsection | 127-132 | Delete (belongs in subsystem-package-manager.md and subsystem-cli.md, where it already is) |
| Full "Project Pattern Consistency" table (14 rows) | 136-154 | Delete (OCX-specific — all rows already in subsystem rules; do a final audit before delete to confirm 0 gaps) |
| "OCX-specific signals of misplaced code" bullet list | 166-170 | Delete |
| "OCX layer guide" table | 171-179 | Delete (belongs in `architecture-principles.md`) |
| "Follows OCX conventions" bullet in Review Checklist | 192 | Replace with generic "Follows established language conventions" |
| "`crates/ocx_lib/`, `crates/ocx_cli/`, and `crates/ocx_mirror/`" in unwrap anti-pattern | 35 | Generalize to "library crates must never use `.unwrap()`" |

**Replace** OCX examples in "SOLID in Rust" table (line 70) with generic examples:

| Principle | OCX example (current) | Generic example (new) |
|---|---|---|
| SRP | `PackageManager` facade delegates to focused task methods | One struct per concern; split `impl` blocks by role |
| OCP | `IndexImpl` trait with `LocalIndex`/`RemoteIndex` | New `impl Trait` instead of new match arms |
| LSP | Trait impl must not panic where contract promises `Result` | Trait impl honors the documented contract |
| ISP | Accept `&[T]` not `Vec<T>` when only reading | Narrowest trait bounds: `impl Write` not `impl Read + Write + Seek` |
| DIP | Constructor takes `impl Client` not `HttpClient` | Depend on `impl Trait`/`dyn Trait`, not concrete types |

### 2.4 Integrate research findings

Add the following **new** block-tier anti-patterns from the research (not currently in rust-quality.md):

- **`anyhow` in library APIs** — `anyhow::Error` destroys downstream `match`-ability. Rule: libs use `thiserror`, binaries use `anyhow`. Using both is fine; mixing the roles is not.
- **Silent error swallowing** — `let _ = result` or `.ok()` without a comment explaining why the error is deliberately ignored.
- **RPIT without `use<..>` bounds in public APIs (edition 2024)** — Rust 2024 implicitly captures all in-scope lifetimes in `impl Trait` return types. For public library functions, add explicit `use<'a, T>` bounds to document and lock the capture set.
- **`Box<dyn Error>` as a function's error return type in lib code** — loses type information; use a concrete enum via `thiserror`.
- **`clippy::correctness` group violations** — deny-by-default for a reason; never suppress without a comment.

Add the following **new** 2026-specific guidance at the end of the file:

```markdown
## 2026 Update Notes

- **Edition 2024 is stable.** Migrate with `cargo fix --edition`. Key impact: RPIT now captures all lifetimes — the `Captures` trick and outlives trick are dead weight, remove them. Reserve the `gen` identifier (it's a keyword even without stable generators).
- **`thiserror` 2.x** is the current line; new projects should pin `>=2`. The `#[error(transparent)]` improvements and better `source` chaining are worth the upgrade.
- **`clippy::pedantic` cherry-picking** — don't enable the whole group, but pick: `clippy::semicolon_if_nothing_returned`, `clippy::match_wildcard_for_single_variants`, `clippy::inefficient_to_string`.
- **`snafu`** — for subsystems with many error sites requiring rich context (file paths, HTTP status codes), `snafu`'s context selector pattern is gaining adoption over `thiserror` + manual `map_err`. Worth evaluating for large internal subsystems, not a blanket replacement.
```

**Target size**: ~150 lines.

### 2.5 Verify OCX content is covered elsewhere

Before deleting the "Project Pattern Consistency" table, audit each of the 14 rows against subsystem rules. Expected: every row already has coverage. If any row has a gap, either (a) add the missing coverage to the right subsystem rule, or (b) keep the row in a new `architecture-principles.md` or `ocx-conventions.md` rule. **This audit is blocking — do not delete until confirmed.**

---

## Phase 3 — Create `quality-python.md`

**Goal**: New shareable Python quality rule, synthesizing `python/SKILL.md` content + research findings.

### 3.1 New file `.claude/rules/quality-python.md`

**Frontmatter**:

```yaml
---
paths:
  - "**/*.py"
  - "**/pyproject.toml"
  - "**/requirements*.txt"
---
```

**Structure**:

```markdown
# Python Code Quality

Python-specific quality guide. Universal principles (SOLID, DRY, YAGNI,
severity tiers) live in `code-quality.md` — this file covers
**Python-specific applications** plus modern tooling and type system.

## Anti-Patterns (Python-Specific)

### Block (must fix before merge)
- Bare `except:` or `except Exception:` — swallows `KeyboardInterrupt`, `SystemExit`. Always name. Ruff: `E722`.
- `assert` for input validation in production — stripped with `-O`. Use `if`/`raise`.
- Mutable default arguments — `def f(x=[])` shares state across calls. Use `None` sentinel. Ruff: `B006`.
- Wildcard imports (`from x import *`) — pollutes namespace, defeats type checkers. Ruff: `F403`.
- `dict[str, Any]` or `TypedDict` without full field coverage at public API boundaries — prevents type narrowing. Use `dataclass`, fully-typed `TypedDict`, or `NamedTuple`.
- Exception chaining dropped — `raise NewError(...)` without `from e` loses the original traceback. Ruff: `B904`.
- `asyncio.gather(*tasks)` for new code — use `asyncio.TaskGroup` (3.11+) for structured concurrency and automatic sibling cancellation.
- `yield` inside `asyncio.TaskGroup` or `asyncio.timeout` context managers — PEP 789: suspending execution transfers cancellation to the wrong task.
- Missing type annotations on public functions — type checkers cannot verify callers. Ruff: `ANN` group.
- Shadowing built-ins (`list`, `dict`, `id`, `type`, `input`).
- `eval()` / `exec()` on user input — injection risk.

### Warn (should fix)
- `type: ignore` without an error code specifier — use `type: ignore[specific-error]` to avoid masking real issues.
- `@runtime_checkable` Protocol used for `isinstance` in hot paths — O(n) in method count via `__dict__` introspection.
- `ABC` where `Protocol` fits — prefer `Protocol` for interfaces at module boundaries and DI, especially with third-party types.
- `dataclass` without `slots=True` on Python 3.10+ — adds `__dict__` overhead.
- `Self` type not used for builder/fluent methods — `def copy(self) -> "MyClass"` breaks for subclasses; use `from typing import Self` (3.11+).
- `match` not used for exhaustive enum dispatch — `if/elif` chains over `Enum` members should be `match` statements so the type checker can warn on non-exhaustive patterns.
- `contextvars.ContextVar` not used for request-scoped state in async code — passing request IDs through every function signature is fragile.

## Type System (Python 3.13+)

- **`Protocol`** over ABC for structural subtyping at module boundaries
- **`Iterable[T]` / `Sequence[T]`** over `list[T]` in function params when only iterating / indexing without mutation
- **`TypedDict` with `Required`/`NotRequired`** (3.11+) instead of `total=False`
- **`Self`** (3.11+) for builder/fluent return types
- **`Final`**, **`Literal`**, **`Never`** where they capture intent
- **Modern generics**: `list[int]` not `List[int]` (3.9+); `X | None` not `Optional[X]` (3.10+)

## Async (Python 3.11+)

- **`asyncio.TaskGroup`** over manual `gather` — structured concurrency, automatic sibling cancellation on failure
- **Cancel safety**: never swallow `asyncio.CancelledError` — always re-raise
- **`contextvars`** for per-task state; auto-propagates through `asyncio.Task` copies

## Modern Tooling (2026)

| Tool | Role | Replaces |
|---|---|---|
| **uv** | Package manager, venv, script runner | pip, virtualenv, poetry, pipx, pyenv |
| **ruff** | Linter + formatter | flake8, black, isort, pylint |
| **pyright** | Type checker (production default) | mypy (for speed and IDE integration) |
| **ty** | Type checker (Astral, Beta 2026) | mypy/pyright (10-60x faster; lacks plugin system) |
| **pytest** + **pytest-asyncio** | Testing | unittest |

2026 recommendation: `uv` + `ruff` + `pyright` is the default stack. Evaluate `ty` for editor speed but keep `pyright` in CI until it gains plugin parity (Django, Pydantic, SQLAlchemy stubs).

## Code Review Checklist (Python-Specific)

- [ ] No bare `except:`, no unnamed `except Exception:`
- [ ] No mutable default arguments
- [ ] No `assert` for runtime validation
- [ ] Exception chaining preserved (`from e` or `from None`)
- [ ] Public functions fully type-annotated
- [ ] `TaskGroup` over `gather` for new async code
- [ ] Modern generics (`list[T]`, `X | None`) not legacy (`List[T]`, `Optional[X]`)
- [ ] Type checker passes (`pyright --strict` or `ty check`)
- [ ] Linter passes (`ruff check`)
- [ ] Formatter applied (`ruff format`)

## Sources

Authoritative references used in this rule:
- [PEP 789 — Preventing task-cancellation bugs in async generators](https://peps.python.org/pep-0789/)
- [Python asyncio docs — TaskGroup](https://docs.python.org/3/library/asyncio-task.html)
- [Python typing spec — Protocols](https://typing.python.org/en/latest/spec/protocol.html)
- [ruff GitHub](https://github.com/astral-sh/ruff)
- [Astral ty announcement](https://astral.sh/blog/ty)
```

**Target size**: ~130 lines.

---

## Phase 4 — Create `quality-typescript.md`

**Goal**: New shareable TypeScript quality rule focused on strict mode, ESM, narrowing.

### 4.1 New file `.claude/rules/quality-typescript.md`

**Frontmatter**:

```yaml
---
paths:
  - "**/*.ts"
  - "**/*.tsx"
  - "**/*.mts"
  - "**/*.cts"
  - "**/tsconfig*.json"
---
```

**Structure**:

```markdown
# TypeScript Code Quality

TypeScript-specific quality guide (TS 5.x, 2026). Universal principles live in
`code-quality.md`. For Vite build tool specifics, see `quality-vite.md`.

## tsconfig.json Strictness Baseline

`strict: true` is mandatory and non-negotiable. The 2026 community consensus
(Total TypeScript, WhatIsLove.dev) adds these flags on top of `strict`:

```jsonc
{
  "strict": true,
  "noUncheckedIndexedAccess": true,       // arr[0] → T | undefined
  "exactOptionalPropertyTypes": true,     // ? means absent, not undefined
  "noPropertyAccessFromIndexSignature": true,
  "verbatimModuleSyntax": true,           // type imports stay type-only in emit
  "moduleResolution": "Bundler",          // for Vite/Bun/esbuild projects
  "module": "ESNext",
  "isolatedModules": true                 // safe for single-file transforms
}
```

`noUncheckedIndexedAccess` is NOT yet part of `strict` (TS issue #49169, open since 2022).
It's the single highest-value flag missing from strict mode — always enable it.

## `any` vs `unknown`

- `unknown` for values whose shape you don't control (API responses, `JSON.parse`, error catches). Narrow before use.
- `any` is acceptable only at deliberate escape hatches in adapter/interop code and test fixtures — never in library surface or domain logic.
- `catch (e)` defaults to `unknown` since TS 4.4 (`useUnknownInCatchVariables`, included in `strict`). Never `catch (e: any)`.

## Anti-Patterns

### Block (must fix before merge)
1. **`any` in exported function signatures** — dissolves the entire type graph downstream.
2. **`as SomeType` to silence a type error** — assertion without narrowing. Use type guard instead.
3. **Non-null assertion `!`** on values that can genuinely be null/undefined at runtime.
4. **`catch (e: any)`** — use default `unknown` and narrow.
5. **`@ts-ignore` without a comment** — comment explaining why is mandatory. Prefer `@ts-expect-error` so the suppression is removed when the underlying issue is fixed.
6. **TypeScript `enum`** — numeric enums are erased at runtime with subtle reverse-mapping bugs. Use `const` unions: `type Direction = "north" | "south"`.
7. **`Object` / `{}` as a type** — use `Record<string, unknown>` or a named interface.
8. **Index signature access without undefined check** — caught by `noUncheckedIndexedAccess`.
9. **Optional property typed as `T | undefined`** instead of `?: T` — caught by `exactOptionalPropertyTypes`.
10. **`eval()` / `Function()` constructor** — injection risk, always find a typed alternative.

### Warn
1. Overusing generics where `unknown` + narrowing suffices.
2. Type predicates (`is`) without airtight runtime checks — false type guards are silent bugs.
3. Intersecting incompatible types with `&` to "merge" them — use `Omit` + spread.
4. `Function` as a type — use explicit signature `(...args: unknown[]) => unknown`.
5. Deeply nested conditional types — split into named aliases.
6. Barrel files (`index.ts`) in library code that impede tree-shaking.

## Type Narrowing Patterns

- **Discriminated unions**: tag every union with a `kind`/`type` literal field. Switch narrows exhaustively.
- **`satisfies` operator** (TS 4.9+): validates a value conforms to a type without widening its inferred type. Pattern: `const config = { … } satisfies Config` instead of `const config: Config = { … }`.
- **`as const`**: freezes literal types. Combine: `const STATUSES = ["open", "closed"] as const satisfies readonly Status[]`.
- **`never` exhaustion**: default branch of a discriminated-union switch assigns to `never` to get compile errors on missing cases.

## Module System (ESM-only in 2026)

- `"type": "module"` in `package.json`
- `verbatimModuleSyntax: true` — forces `import type` for type-only imports
- `moduleResolution: "Bundler"` for Vite, Bun, esbuild — do not use `"node16"` unless targeting Node without a bundler
- `.mts` / `.cts` extensions: only when mixing ESM and CJS in the same package

## 2026 Features Worth Knowing

- **`using` / `await using`** (TS 5.2): Explicit Resource Management. Objects implementing `Symbol.dispose` are automatically disposed at scope exit. Use for file handles, DB connections, test teardown.
- **Import attributes** (TS 5.3): `import data from "./data.json" with { type: "json" }`. Replaces old `assert` syntax. Needed for JSON module imports in native ESM.
- **Standard decorators** (TS 5.0): Do NOT use `experimentalDecorators: true` in new code — that's the pre-standard legacy implementation.

## Tooling (2026)

| Tool | Status | Use when |
|---|---|---|
| **Biome** | Recommended default | New projects, ~85% ESLint rule coverage, 10-25x faster |
| **ESLint + @typescript-eslint** | Still required | Type-aware rules, framework plugins (react, vue, svelte), custom rules |
| **Oxlint** | Emerging | Experimental speed option; keep alongside ESLint |
| **tsc** | Type checking only | Never as build tool in Vite/Bun projects — use `tsc --noEmit` in CI |
| **Bun** | Type-stripping runtime | Pair with `tsc --noEmit` in CI for actual checking |

2026 recommendation: **Biome** for formatting + basic linting, **`tsc --noEmit`** in CI for full type checking. Keep ESLint only when you need its plugin ecosystem.

## Code Review Checklist (TypeScript-Specific)

- [ ] `strict: true` + the 2026 strict-baseline flags in tsconfig
- [ ] No `any` in exported signatures
- [ ] No `as X` assertions bypassing narrowing
- [ ] No non-null `!` without justification comment
- [ ] `catch (e)` narrows from `unknown`, not `any`
- [ ] Unions are discriminated, switch has `never` exhaustion check
- [ ] `satisfies` used for config objects
- [ ] Import type syntax used consistently
- [ ] No TypeScript `enum`; const unions instead
- [ ] `tsc --noEmit` passes, Biome/ESLint passes

## Sources

- [TypeScript TSConfig Reference](https://www.typescriptlang.org/tsconfig/)
- [noUncheckedIndexedAccess GitHub issue #49169](https://github.com/microsoft/TypeScript/issues/49169)
- [Total TypeScript: Configuring TypeScript](https://www.totaltypescript.com/books/total-typescript-essentials/configuring-typescript)
- [2ality: satisfies operator](https://2ality.com/2025/02/satisfies-operator.html)
- [2ality: TypeScript enum patterns](https://2ality.com/2025/01/typescript-enum-patterns.html)
- [TypeScript 5.2 release notes: using declarations](https://www.typescriptlang.org/docs/handbook/release-notes/typescript-5-2.html)
```

**Target size**: ~150 lines.

---

## Phase 5 — Create `quality-bash.md`

**Goal**: New shareable Bash quality rule focused on safety-critical scripting.

### 5.1 New file `.claude/rules/quality-bash.md`

**Frontmatter**:

```yaml
---
paths:
  - "**/*.sh"
  - "**/*.bash"
---
```

**Structure** (target ~110 lines):

```markdown
# Bash Script Quality

Bash-specific quality guide. Universal principles live in `code-quality.md`.

## Required Script Header

Every Bash script in production MUST start with:

```bash
#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
```

- `-e`: exit on any command returning non-zero
- `-u`: treat unset variables as errors (catches typos)
- `-o pipefail`: pipe fails if any stage fails, not just the last
- `IFS=$'\n\t'`: prevents word-splitting on spaces in filenames

Add a cleanup trap for any script that creates temp files or needs teardown:

```bash
cleanup() { rm -f "$tmpfile"; }
trap cleanup EXIT
```

`EXIT` fires on both normal exit and error — prefer over individual `ERR`/`INT`/`TERM` traps for general cleanup.

## Anti-Patterns

### Block (must fix before merge)
1. **Missing `set -euo pipefail`** — silently continues after errors
2. **Unquoted `$var`** in command position — word splits and globs unexpectedly
3. **`rm -rf $dir`** with unquoted/potentially-empty variable — catastrophic on empty string
4. **`eval` on user input** — shell injection
5. **Storing structured data as strings** and parsing with `grep`/`sed` — use arrays or JSON tools
6. **Reading filenames with `for f in $(ls)`** — breaks on spaces. Use `for f in ./*` or `find`
7. **Piping into `while read`** and assigning variables — subshell scope loss. Use process substitution: `while read; do …; done < <(cmd)`
8. **`if [ $? -eq 0 ]`** — use `if cmd; then` directly
9. **`[ ]` instead of `[[ ]]`** for string comparisons in Bash — `[ ]` has POSIX word-splitting risks
10. **Ignoring shellcheck warnings** without inline directive + explanation
11. **`rm`/`grep`** without `-- ` separator before user-supplied variable — variable starting with `-` is treated as flag
12. **Missing `local`** in function-scoped variables — globally mutable

### Warn
1. Heredocs without `<<'EOF'` when interpolation is unintended
2. Temporary files without `mktemp` — collisions and symlink attacks
3. Scripts over 200 lines without function decomposition
4. No `main` function — all code at top level, no clear entry point
5. `pipefail` + `grep` gotcha — grep exits 1 on no match, which `pipefail` propagates. Use `|| true` when that's intentional
6. Associative arrays without Bash version check (`bash >=4`)

## Quoting Rules

- ALWAYS double-quote variable expansions: `"$var"`, `"${var}"`, `"${arr[@]}"`
- `"$var"` for brevity; `"${var}"` when needed to disambiguate: `"${var}_suffix"`
- `"$(cmd)"` over backticks — nestable and readable
- Glob patterns in conditions are NOT quoted: `[[ $file == *.txt ]]`
- Arrays: `"${arr[@]}"` preserves each element as a separate argument; `"${arr[*]}"` collapses to one string

## Shellcheck Discipline

- Run at `shellcheck -S warning` — treats `warning` and `error` as blocking; `info`/`style` advisory
- Never blanket-suppress. Use inline directives with comments:
  ```bash
  # shellcheck disable=SC2034  # intentionally unused, sourced externally
  ```
- Legitimate to suppress: `SC1091` (can't follow sourced file), `SC2154` (variable defined in sourced file)
- Never suppress: `SC2086` (unquoted variable), `SC2068` (array expansion without quotes), `SC2046` (unquoted command substitution)
- `shfmt -i 4 -ci` for formatting (4-space indent, switch cases aligned)

## POSIX sh vs Bash

| Use POSIX sh (`#!/bin/sh`) when... | Use Bash when... |
|---|---|
| Minimal containers (Alpine, distroless) | You need arrays, associative arrays, `[[ ]]`, `(( ))` |
| Cross-platform portability (BSD, macOS `/bin/sh` is dash) | You need process substitution, `mapfile`, `nameref` |
| Short glue scripts with no complex logic | Long-lived scripts benefiting from Bash-specific safety features |

For CI/CD (GitHub Actions), Bash is always available — use it. For Docker `ENTRYPOINT` in minimal images, use POSIX sh or install Bash explicitly.

## Testing

- **bats-core**: TAP-compliant, simple, widely used in CI. Good for integration tests.
- **ShellSpec**: BDD-style, pure POSIX, supports mocking and parameterized tests. Better for unit-level isolation.

## Code Review Checklist (Bash-Specific)

- [ ] Script starts with `#!/usr/bin/env bash`, `set -euo pipefail`, `IFS=$'\n\t'`
- [ ] All variable expansions quoted
- [ ] Functions use `local` for scoped variables, `readonly` for constants
- [ ] `trap … EXIT` for cleanup when temp files or resources acquired
- [ ] `shellcheck` and `shfmt` both pass
- [ ] No `eval` or `$(…)` on unsanitized input
- [ ] Arrays used for command arg construction, not string concatenation

## Sources

- [ShellCheck wiki](https://github.com/koalaman/shellcheck/wiki)
- [Shell Script Best Practices (sharats.me)](https://sharats.me/posts/shell-script-best-practices/)
- [ShellSpec documentation](https://shellspec.info/why.html)
```

**Target size**: ~110 lines.

---

## Phase 6 — Create `quality-vite.md` (and delete the 732-line skill)

**Goal**: A tiny, opinionated, shareable Vite rule — not a cookbook.

### 6.1 New file `.claude/rules/quality-vite.md`

**Frontmatter**:

```yaml
---
paths:
  - "**/vite.config.*"
  - "**/vitest.config.*"
  - "**/.vitepress/config.*"
---
```

**Structure** (target ~100 lines):

```markdown
# Vite Build Tool Quality

Vite-specific opinions for 7.x/8.x (2026). Not a cookbook — for config
examples and API reference, read [vite.dev](https://vite.dev/).

## Anti-Patterns

### Block (must fix before merge)
1. **Hardcoded credentials or secrets** in `vite.config.*`
2. **`VITE_` prefix on server-only env vars** — exposes them to the client bundle at build time. Never prefix secrets with `VITE_`.
3. **Browser API at module scope** in components used in VitePress/SSR — crashes the pre-render. Guard with `import.meta.env.SSR` or use `<ClientOnly>`.
4. **`vite.config.ts` at root alongside `.vitepress/config.ts`** — VitePress ignores the root config; the split causes silent override bugs.
5. **`build.target: "es5"`** with Rolldown (Vite 8) — not supported. Rolldown targets modern JS only.
6. **Mixing SSR and client globals** in shared modules — pre-render crashes.
7. **Missing strict env var validation** at config load — validate with zod/valibot; fail the build on invalid env.

### Warn
1. Missing `base` option for non-root deployments
2. `optimizeDeps.include` as a workaround instead of fixing the underlying ESM incompatibility
3. Using `vite-plugin-*` wrappers that duplicate Vite 8 built-ins (e.g., `vite-tsconfig-paths` is now built-in)
4. Not committing `.gitignore` entry for `.vitepress/cache`
5. Enabling experimental `fullBundleMode` in production — not stable
6. Not specifying `build.outDir` explicitly — defaults differ between app mode and library mode
7. `resolve.alias` with absolute paths — use `fileURLToPath(new URL(..., import.meta.url))` for portability

## Env Var Discipline

- `VITE_*` prefix is the **client exposure** switch — anything with this prefix ends up in the browser bundle
- `.env.local` gitignored; `.env` committed (NO secrets, only public defaults)
- Validate env vars at config load with zod/valibot; fail fast on missing/invalid values
- Never read `process.env` at module scope in client code — it's resolved at build time, not runtime

## Vite 7 → Vite 8 (2026) Migration

- **Rolldown replaces Rollup** for production builds in Vite 8 — 10-30x faster
- Internal plugins (`alias`, `resolve`) are now Rust-native via `nativePlugins: 'v1'`
- `resolve.tsconfigPaths: true` built-in — drop `vite-tsconfig-paths` plugin
- Node.js baseline: 20.19+ / 22.12+ (Vite 7+); 18 is EOL for Vite
- Default browser target changed: `'baseline-widely-available'` instead of `'modules'`

## VitePress-Specific

1. **SSR compatibility is mandatory** — VitePress pre-renders in Node at build time. Any browser API accessed at import time crashes the build.
2. **`<ClientOnly>` wrapper** for components that cannot be made SSR-safe.
3. **`defineClientComponent`**: VitePress helper for Vue components that use browser APIs — avoids dynamic-import boilerplate.
4. **Vite config lives in `.vitepress/config.ts`**, not at root. The `vite` key inside VitePress config accepts the same `UserConfig` shape.
5. **Full typing**: use `defineConfig` from `vitepress` — it catches malformed nav/sidebar/theme configs at author time.

## Config Structure Recommendations

```typescript
// .vitepress/config.ts or vite.config.ts
import { defineConfig } from "vite";

export default defineConfig(({ command, isSsrBuild }) => ({
  build: {
    target: "baseline-widely-available", // Vite 7+ default, explicit for visibility
  },
}));
```

- Use the function form of `defineConfig` only when config needs to branch on `command` or `isSsrBuild`
- Library mode: set `build.lib` with `entry`, `formats`, `fileName`. App-mode config for libraries is wrong — tree-shaking and externalization behave differently
- Extract reusable plugin arrays to a local helper — do not duplicate between `vitest.config.ts` and `vite.config.ts`

## Code Review Checklist (Vite-Specific)

- [ ] No secrets in `vite.config.*`
- [ ] No `VITE_` prefix on server-only env vars
- [ ] Env vars validated at config load
- [ ] `base` set for non-root deployments
- [ ] Browser API access guarded for SSR if VitePress
- [ ] No duplicate config files (root `vite.config.ts` + `.vitepress/config.ts`)

## Sources

- [Vite 8.0 announcement](https://vite.dev/blog/announcing-vite8)
- [Vite 7.0 announcement](https://vite.dev/blog/announcing-vite7)
- [Rolldown integration guide](https://v7.vite.dev/guide/rolldown)
- [VitePress SSR Compatibility](https://vitepress.dev/guide/ssr-compat)
```

**Target size**: ~110 lines.

### 6.2 Delete the 732-line `vite/SKILL.md`

The skill covered install, config, environment, dev server, build, CSS, assets, preview mode, complete production config, and feedback loops. All of that content is available at [vite.dev](https://vite.dev/) in authoritative form. Deleting the skill is the right call — the quality rule above captures the **opinions** (what to avoid, what to prefer), and vite.dev provides the **reference**.

---

## Phase 7 — Delete the 5 language skill directories

**Goal**: Remove the skill directories and their metadata entries.

### 7.1 Delete skill directories

```sh
rm -rf .claude/skills/python
rm -rf .claude/skills/rust
rm -rf .claude/skills/typescript
rm -rf .claude/skills/bash
rm -rf .claude/skills/vite
```

### 7.2 Remove entries from `skill-rules.json`

Delete the 5 corresponding entries (`python`, `rust`, `typescript`, `bash`, `vite`). Remaining count: **15 skills**.

### 7.3 Update `post_tool_use_tracker.py`

No changes expected — the hook references paths like `.claude/skills/builder/SKILL.md`, none of the language skills. Verify with grep before concluding.

### 7.4 Update suggestion hook behavior

The suggestion hook reads `skill-rules.json` at runtime — no code change needed. After the JSON entries are removed, suggestions for `rust`, `python`, `typescript`, `bash`, `vite` keywords will disappear automatically.

---

## Phase 8 — Update cross-references to `rust-quality.md`

**Goal**: Rename references from `rust-quality.md` to `quality-rust.md` across all files that cite it.

Files that reference `rust-quality.md` (15 files per exploration):

1. `.claude/rules/code-quality.md` (line 3) — will be rewritten in Phase 1
2. `.claude/rules/swarm-workers.md`
3. `.claude/rules/feature-workflow.md`
4. `.claude/agents/worker-builder.md` (after Phase G preamble)
5. `.claude/agents/worker-reviewer.md` (after Phase H inline checklist)
6. `.claude/agents/worker-tester.md` (after Phase G preamble)
7. `.claude/skills/builder/SKILL.md`
8. `.claude/skills/code-check/SKILL.md` (5 occurrences)
9. `.claude/skills/swarm-review/SKILL.md` (4 occurrences)
10. `.claude/skills/maintain-ai-config/SKILL.md` (if any — verify)
11. `.claude/skills/swarm-execute/SKILL.md` (if any — verify)
12. `CLAUDE.md` (if any — verify)
13. `.claude/tests/test_ai_config.py` (no direct string; tests glob rule files)
14. `.claude/hooks/post_tool_use_tracker.py` (grep to confirm absence)
15. Any `feedback_*.md` historical artifact — **do not rewrite body**; add a historical note if needed (same approach as Phase 1 flatten)

Use `grep -rln "rust-quality.md" .claude CLAUDE.md` to find all occurrences. Update in-place with Edit, preserving surrounding context.

Also update cross-references **pointing into the language skills that we're about to delete**: any skill or rule that mentions `python/SKILL.md`, `rust/SKILL.md`, `typescript/SKILL.md`, `bash/SKILL.md`, `vite/SKILL.md` — replace with a reference to the corresponding `quality-*.md` rule, or remove the reference entirely if it was a suggestion pointer.

---

## Phase 9 — Update `CLAUDE.md`

**Goal**: Fold OCX-specific taskfile conventions into CLAUDE.md's Build section, since Phase 1 strips them from code-quality.md.

### 9.1 Add "Taskfile conventions" subsection

Under the existing "Build & Development Commands" section in CLAUDE.md, add three bullets:

```markdown
### Taskfile conventions

- **Composite tasks aggregate subtasks.** A quality-gate task (e.g., `lint:shell`) calls individual tool tasks (`shell:lint`, `shell:format:check`), not raw tool invocations.
- **Individual tools get their own subtask.** Each linter/formatter is a separate task so it can be run independently for debugging.
- **`task verify` references composite tasks only.** Keep the verify pipeline simple — one entry per concern, not one per tool.
```

Verify CLAUDE.md stays under 200 lines after the addition. If it bloats, create `.claude/rules/taskfile-conventions.md` scoped to `**/taskfile*.yml, **/taskfiles/**` instead (not ideal — CLAUDE.md is preferred so the guidance is always loaded).

---

## Phase 10 — Update `meta-ai-config.md`

**Goal**: Document the new shareable quality-rules convention.

### 10.1 Update the Rules sub-block

Add a new bullet to the Rules section (currently lines 55-60):

```markdown
### Rules (`.claude/rules/*.md`)

- `paths:` frontmatter for scoped rules; omit for global
- <200 lines. If longer, split by domain.
- Structure: types → invariants → gotchas → cross-refs
- Dead glob detection: after renaming directories, verify `paths:` patterns still match files
- **Shareable quality rules** use the `quality-*.md` naming convention (e.g., `quality-rust.md`, `quality-python.md`). These must be **project-independent** — no references to OCX types, modules, or conventions. OCX-specific Rust patterns live in `subsystem-*.md`. Shareable rules use broad `paths:` globs (e.g., `**/*.rs`) so they activate regardless of project layout.
```

### 10.2 Add a new anti-pattern

Add to the Anti-Patterns list (items 1-9 after previous commit, new item 10):

```markdown
10. **Language rules with project-specific content** — `quality-{lang}.md` files are shareable across repos. Any reference to OCX types (`PackageErrorKind`, `ReferenceManager`) or modules (`ocx_lib/`, `crates/ocx_mirror/`) belongs in `subsystem-*.md`, not in a language rule. Enforced structurally by `test_quality_rules_no_ocx_leak`.
```

---

## Phase 11 — Update structural tests in `.claude/tests/test_ai_config.py`

**Goal**: Enforce the new architecture invariants.

### 11.1 Add test: no OCX leak into shareable rules

```python
class TestShareableQualityRules:
    """Quality rules must be project-independent for cross-repo sharing."""

    _OCX_FORBIDDEN_STRINGS = [
        "PackageErrorKind",
        "ReferenceManager",
        "PackageManager",
        "ocx_lib",
        "ocx_cli",
        "ocx_mirror",
        "to_relaxed_slug",
        "DIGEST_FILENAME",
        "crates/ocx",
        "DirWalker",
    ]

    _SHAREABLE_RULES = [
        "code-quality.md",
        "quality-rust.md",
        "quality-python.md",
        "quality-typescript.md",
        "quality-bash.md",
        "quality-vite.md",
    ]

    def test_shareable_rules_no_ocx_leak(self) -> None:
        """Shareable quality rules must not reference OCX-specific names."""
        violations = []
        for name in self._SHAREABLE_RULES:
            path = CLAUDE_DIR / "rules" / name
            if not path.exists():
                continue  # rule not yet created
            text = path.read_text()
            for forbidden in self._OCX_FORBIDDEN_STRINGS:
                if forbidden in text:
                    violations.append((name, forbidden))
        assert not violations, (
            f"Shareable quality rules contain OCX-specific strings: {violations}. "
            f"OCX-specific Rust patterns belong in subsystem-*.md rules, not in "
            f"shareable quality rules (see meta-ai-config.md Anti-Pattern #10)."
        )
```

### 11.2 Add test: all quality rules are path-scoped

```python
    def test_all_quality_rules_have_paths_frontmatter(self) -> None:
        """Every quality-{lang}.md rule must be path-scoped to its language files."""
        missing = []
        for name in self._SHAREABLE_RULES:
            if name == "code-quality.md":
                continue  # root is global, not scoped
            path = CLAUDE_DIR / "rules" / name
            if not path.exists():
                continue
            paths_in_frontmatter = TestRuleGlobs._extract_paths(path)
            if not paths_in_frontmatter:
                missing.append(name)
        assert not missing, (
            f"Quality rules missing `paths:` frontmatter: {missing}. "
            f"Language quality rules must be path-scoped so they only load "
            f"when editing that language."
        )
```

### 11.3 Add test: no deleted language skills referenced

```python
    def test_no_references_to_deleted_language_skills(self) -> None:
        """After the reorg, no files should reference the removed language skills."""
        deleted_skills = [
            "skills/python/SKILL.md",
            "skills/rust/SKILL.md",
            "skills/typescript/SKILL.md",
            "skills/bash/SKILL.md",
            "skills/vite/SKILL.md",
        ]
        violations = []
        for rule_file in CLAUDE_DIR.rglob("*.md"):
            # Skip historical artifacts — they preserve old references intentionally
            if "artifacts" in rule_file.parts:
                continue
            text = rule_file.read_text()
            for deleted in deleted_skills:
                if deleted in text:
                    violations.append((str(rule_file.relative_to(ROOT)), deleted))
        assert not violations, (
            f"Files reference deleted language skills: {violations}. "
            f"Update to reference the corresponding quality-*.md rule."
        )
```

### 11.4 Fix or remove obsolete tests

- The existing `test_security_auditor_artifact_path` is already fine (post-flatten).
- Review other tests that may reference `.claude/skills/{python,rust,typescript,bash,vite}`. If any exist, update or delete.

---

## Phase 12 — Verification and checkpoint

### 12.1 Verification gate

Run in this order:

1. `task lint:ai-config` — expect 28 tests passing (25 from previous commits + 3 new from Phase 11).
2. Visual spot-check of `.claude/rules/`: list files with `ls -la` and confirm the new structure matches the target architecture.
3. `task verify` — full quality gate. If any test fails, do NOT proceed — diagnose and fix.
4. **Manual recursion check**: grep for `rust-quality.md` across the entire `.claude/` directory and CLAUDE.md. Should return zero matches (except in historical artifacts).

### 12.2 Checkpoint

Use `task checkpoint` to amend all changes into a single Checkpoint commit (per Michael's workflow). Michael will decide the final conventional commit message when landing.

Proposed final message for the squash:

```
chore(claude): reorganize quality rules as shareable scoped language rules

Convert the 5 language "skills" (python, rust, typescript, bash, vite)
into path-scoped quality-{lang}.md rules under .claude/rules/. Rename
rust-quality.md → quality-rust.md for consistency, strip OCX-specific
content (already covered in subsystem-*.md rules), and integrate 2026
best-practice findings (Rust edition 2024, Python TaskGroup/uv/ruff/ty,
TypeScript strict baseline, Bash shellcheck, Vite 8 Rolldown).

code-quality.md becomes the shareable cross-language root; OCX-specific
taskfile conventions move into CLAUDE.md. Add 3 structural tests
enforcing the shareable invariant (no OCX leak, path-scoped, no
references to deleted skills).

Skills: 20 → 15. 5 SKILL.md files deleted, 5 rules created.
```

---

## Files Touched (Full List)

### Created
- `.claude/rules/quality-python.md` (~130 lines)
- `.claude/rules/quality-typescript.md` (~150 lines)
- `.claude/rules/quality-bash.md` (~110 lines)
- `.claude/rules/quality-vite.md` (~110 lines)

### Renamed
- `.claude/rules/rust-quality.md` → `.claude/rules/quality-rust.md` (via `git mv`, then stripped from 194 to ~150 lines)

### Modified
- `.claude/rules/code-quality.md` — strip OCX Quality Gates section; add See Also; keep Two Hats. 146 → ~110 lines.
- `CLAUDE.md` — add Taskfile Conventions subsection under Build & Development Commands (≤10 lines added; verify <200 total)
- `.claude/rules/meta-ai-config.md` — Rules sub-block bullet for shareable quality rules; anti-pattern #10
- `.claude/rules/swarm-workers.md` — update reference `rust-quality.md` → `quality-rust.md`
- `.claude/rules/feature-workflow.md` — same
- `.claude/agents/worker-builder.md` — same
- `.claude/agents/worker-reviewer.md` — same
- `.claude/agents/worker-tester.md` — same
- `.claude/skills/builder/SKILL.md` — same
- `.claude/skills/code-check/SKILL.md` — 5 occurrences
- `.claude/skills/swarm-review/SKILL.md` — 4 occurrences
- Other files revealed by `grep -rln "rust-quality.md"`
- `.claude/skills/skill-rules.json` — remove 5 entries (python, rust, typescript, bash, vite)
- `.claude/tests/test_ai_config.py` — 3 new tests in new `TestShareableQualityRules` class

### Deleted
- `.claude/skills/python/` (directory)
- `.claude/skills/rust/` (directory)
- `.claude/skills/typescript/` (directory)
- `.claude/skills/bash/` (directory)
- `.claude/skills/vite/` (directory)

---

## Verification

1. `task lint:ai-config` — **28 tests pass** (25 from previous flatten + protocol commits + 3 new).
2. `task verify` — full quality gate (format, clippy, lint, license, build, unit tests, acceptance tests).
3. **Manual check**: after session restart, verify:
   - Skill tool lists **15** project skills (no language skills).
   - Opening a `.rs` file auto-loads `quality-rust.md` via `paths:` scoping.
   - Opening a `.py` file auto-loads `quality-python.md`.
   - Opening a `.ts`/`.tsx` file auto-loads `quality-typescript.md`.
   - Opening a `.sh` file auto-loads `quality-bash.md`.
   - Opening `.vitepress/config.*` auto-loads `quality-vite.md`.
4. `grep -rln "rust-quality.md" .claude CLAUDE.md` returns zero matches outside `artifacts/`.
5. `grep -rln "skills/\(python\|rust\|typescript\|bash\|vite\)" .claude CLAUDE.md` returns zero matches outside `artifacts/`.
6. `git status` clean after checkpoint.

---

## Deferred / Out of Scope

- **Sharing mechanism across repos** — Michael said *"I don't really care"* about how the sharing works mechanically (copy-paste, submodule, `~/.claude/rules/` user-global, symlinks). The immediate goal is making the content project-independent; a follow-up task can address the transport mechanism.
- **OCX architecture-principles.md extraction** — the "Project Pattern Consistency" table currently in `rust-quality.md` could alternatively move into a new `architecture-principles.md` file or an expanded existing rule rather than being distributed across subsystem rules. Decision: verify the 14 rows are all already covered in subsystem rules (Phase 2.5); if any are not, add a minimal `architecture-principles.md` (or extend the existing one) rather than bloat subsystem rules.
- **Vite cookbook salvage** — the 732-line `vite/SKILL.md` contains some genuinely useful patterns (proxy config, code splitting strategies, asset handling). They're all documented at vite.dev, so deletion loses nothing authoritative. If any passage is project-specific to OCX's website, extract that passage into `subsystem-website.md` before deleting. Verify in Phase 6.2.
- **Python subsystem-tests.md interaction** — OCX's Python usage is primarily acceptance tests (pytest) + hooks (stdlib). `subsystem-tests.md` already covers the pytest-specific OCX test infrastructure. `quality-python.md` must not duplicate that content — the split is: language-intrinsic rules in `quality-python.md`, OCX test fixtures/runners/conventions in `subsystem-tests.md`. Review for overlap in Phase 3.
- **`code-check` skill updates** — the `code-check` skill currently references `rust-quality.md` (5 times). Some of those references may cite sections that are being deleted (the OCX "Project Pattern Consistency" table, the OCX layer guide). Review all 5 references during Phase 8 and update them to point at the appropriate destination (subsystem rule or `quality-rust.md`).
- **`swarm-review` skill updates** — same as `code-check`; 4 references to audit.
- **Plan directory clutter** — unrelated.

---

## Open Questions (resolved via this plan or deferred)

| Q | Status | Decision |
|---|---|---|
| Vite → subsystem-website.md or quality-vite.md? | Resolved | `quality-vite.md` (shareable, ~110 lines) + delete the 732-line skill. Rationale: subsystem-website.md is OCX-specific and violates the shareable goal. |
| Bash rule? | Resolved | Yes — shareable `quality-bash.md`. Michael: *"In future it may get bigger and it's kind of shareable"*. |
| Naming: `quality-rust.md` vs `rust-quality.md`? | Resolved | `quality-rust.md` — Michael: *"I feel like quality minus rust is a little bit more concise and it will keep all quality rules together"*. |
| Split into plan artifact first? | Resolved | Yes — this plan file is the artifact. |
| Commit strategy? | Resolved | Single squashed commit — Michael: *"feel free to organize it more like a backup strategy and review strategy rather than commits that land on main"*. Use `task checkpoint` between phases. |
| OCX-specific Rust content destination? | Resolved | Subsystem rules (verified to already cover it). Delete from rust-quality.md. |
| Shareable root approach? | Resolved | `code-quality.md` stays global (no paths), strips OCX quality gates, adds See Also section pointing to `quality-*.md` leaves. |
