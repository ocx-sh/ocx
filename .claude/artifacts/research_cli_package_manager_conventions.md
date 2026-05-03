# Research: CLI Package Manager Conventions

**Date:** 2026-04-28
**Author:** worker-researcher
**Scope:** Informs OCX `ocx.toml`-driven project-toolchain CLI design — specifically `ocx add`, `ocx remove`, `ocx update`, `ocx lock`, `ocx init`, and default-tag policy.
**Does NOT design:** `ocx add` syntax — that is Unit 7's responsibility. This artifact informs that design.

---

## 1. Comparison Table

| Tool | `add` verb | `remove` verb | `update` verb | Separate `lock` verb? | `init` output | Default version (no constraint) |
|------|-----------|---------------|--------------|----------------------|---------------|----------------------------------|
| **cargo** | `cargo add <crate>` | `cargo remove <crate>` | `cargo update [pkg…]` — rewrites Cargo.lock | `cargo generate-lockfile` exists but `cargo update` is the practical command — **effectively no separate lock verb** | `cargo init` / `cargo new` — Cargo.toml + src/ (non-interactive) | `^x.y.z` of latest release (SemVer caret) |
| **npm** | `npm install <pkg>` | `npm uninstall <pkg>` | `npm update [pkg…]` | **No** — lockfile auto-managed | `npm init` — interactive questionnaire → package.json | `^x.y.z` of `latest` registry tag |
| **yarn v4** | `yarn add <pkg>` | `yarn remove <pkg>` | `yarn up <pkg>` | **No** — yarn.lock auto-updated | `yarn init` — interactive | `^x.y.z` of `latest`; configurable via `defaultSemverRangePrefix` |
| **pnpm** | `pnpm add <pkg>` | `pnpm remove <pkg>` | `pnpm update [pkg…]` | **No** — pnpm-lock.yaml auto-managed | `pnpm init` — minimal package.json | `^x.y.z` of latest |
| **bun** | `bun add <pkg>` | `bun remove <pkg>` | `bun update [pkg…]` | **No** — bun.lock auto-managed | `bun init` — interactive | `^x.y.z` of latest; `--exact` pins exact |
| **poetry** | `poetry add <pkg>` | `poetry remove <pkg>` | `poetry update [pkg…]` | **YES — `poetry lock`** re-resolves without installing | `poetry init` — interactive pyproject.toml | `^x.y.z` of latest |
| **uv** | `uv add <pkg>` | `uv remove <pkg>` | `uv lock --upgrade[-package <pkg>]` (no top-level `update`) | **YES — `uv lock`** is the canonical resolve command | `uv init` — non-interactive pyproject.toml | `>=x.y.z` lower-bound of latest compatible |
| **pip-tools** | Edit requirements.in → `pip-compile` | Edit requirements.in → `pip-compile` | `pip-compile --upgrade[-package pkg]` | `pip-compile` IS the lock — **no add/remove verbs** | No `init` — fully manual | Unpinned in .in; pip-compile → exact in .txt |
| **go mod** | `go get <module>` | `go get <module>@none` | `go get -u ./...` | **No traditional lock** — MVS + go.sum provides determinism; `go mod tidy` for cleanup | `go mod init <module>` — empty go.mod | `@upgrade` = latest release; exact version in go.mod |
| **gradle** | Edit build.gradle (no verb) | Edit build.gradle (no verb) | `./gradlew dependencies --update-locks [spec]` | `--write-locks` flag on `dependencies` task | `gradle init` — interactive template wizard | No default — explicit version required |
| **pixi** | `pixi add <pkg>` | `pixi remove <pkg>` | `pixi update [pkg…]` | **No** — pixi.lock auto-managed | `pixi init` — pixi.toml | Latest major (SemVer pinning strategy; configurable) |
| **mise** | `mise use <tool>` | `mise unuse <tool>` | `mise upgrade [tool…]` | **YES — `mise lock [tool…]`** exists | `mise init` — mise.toml | `@latest` (fuzzy, e.g. `"20"`; `--pin` for exact) |

---

## 2. Convention Summary

### Verb naming consensus

**add / remove**: Universal across all modern tools. No variation. Both always update the lockfile atomically as part of the operation — no tool requires a manual lock step after add/remove.

**update**: Dominant verb for "bump to newer version." Used by cargo, npm, pnpm, bun, pixi, poetry (6 of 12 tools). Notable outliers: uv surfaces upgrade inside `uv lock --upgrade-package`; yarn uses `yarn up`; mise uses `upgrade`; go mod uses `go get -u`.

**lock (separate verb)**: Two camps:

| Camp | Tools | Contract |
|------|-------|---------|
| No separate `lock` | npm, yarn, pnpm, bun, pixi, cargo | Lockfile is a side-effect. `install` (no args) regenerates from manifest. No standalone `lock` command. |
| Explicit `lock` verb | poetry, uv, mise | `lock` = re-resolve to lockfile **without installing**. Decouples resolution from environment mutation. CI-oriented workflow. |

**init**: Universal. Always creates a manifest file. Interactive in npm/yarn/bun/poetry/gradle; non-interactive template in uv/pixi/cargo/mise.

### Default version policy consensus

Strong consensus: "no version specified" resolves against latest published release as a version range. **Zero surveyed tools error on missing version.** The breakdown:

- **`^x.y.z` caret of latest** (dominant): cargo, npm, yarn, pnpm, bun, poetry — 6 of 12
- **SemVer-pinned to latest major**: pixi
- **`>=x.y.z` lower-bound**: uv (Python ecosystem culture)
- **`@upgrade` = latest, pinned exact**: go mod (MVS makes ranges unnecessary)
- **`@latest` fuzzy tag**: mise (tool-version-manager semantics)
- **No default — explicit required**: gradle, pip-tools (build-tool outliers)

### Lockfile mutation on add/remove

Every tool with a lockfile rewrites it **completely** on add/remove. No partial-lockfile update exists in any surveyed tool. The lockfile is always a complete consistent snapshot after any mutation command.

---

## 3. Key Findings

- **`:latest` as default-tag is the universal industry expectation.** npm, yarn, bun document resolving to the `latest` tag explicitly; cargo resolves "latest release"; mise `use` defaults to `@latest`. No tool errors on missing version. OCX erroring on missing tag violates universal user expectation with no precedent.
- **`update` is the correct primary bump verb** — used by 6+ tools, matches user intuition. The user's challenge is validated by industry data.
- **Both `update` and `lock` can coexist with distinct semantics.** Poetry's model is the clearest prior art: `update` bumps + installs; `lock` re-resolves lockfile without touching installed environment. Valuable for CI-first tools.
- **Selective vs full update is universal.** `update <pkg>` = bump one; `update` (no args) = bump all within manifest constraints. Cargo, npm, pnpm, bun, pixi, poetry all implement this identically.
- **add/remove always atomically rewrite the full lockfile.** No tool does partial lockfile mutation. Partial updates risk graph inconsistency.
- **init = minimal manifest, non-interactive preferred for backend tools.** uv/pixi/cargo all produce minimal non-interactive manifests. Interactive is the JS-world default; OCX's backend-first principle favors the non-interactive model.

---

## 4. Design Patterns Worth Considering

**Atomic full-lockfile rewrite on add/remove (universal)**: ocx add and ocx remove should always write a complete fresh ocx.lock. No partial updates — it is an antipattern with no precedent.

**Selective update via positional args (universal)**: `ocx update cmake` = bump only cmake; `ocx update` (no args) = bump all. This behavioral contract is internalized by users of every major package manager.

**`lock` as re-resolve-only, no install (poetry/uv/mise pattern)**: Valuable for OCX's CI-first positioning. "Update the lockfile for the next runner without triggering installs" is a real CI workflow need.

**`latest` as implicit default tag**: When `ocx.toml` has `cmake` (no tag), resolve `:latest`. Log the resolved digest/tag at resolution time so users can audit what "latest" meant.

**Non-interactive `init` with minimal manifest**: `ocx init` should produce a minimal `ocx.toml` with a registry declaration and empty `[tools]` table. Backend-first principle: programmatic creation is the common case.

---

## 5. Industry Context & Trends

**Trending**: uv (astral.sh) — sub-second lock times, displacing poetry and pip-tools rapidly. Its `uv lock` / `uv sync` separation of resolution from installation is the most architecturally clean split in the survey. Strong adoption signal even if OCX doesn't adopt uv's exact verb choices.

**Established**: npm/yarn/pnpm model. `add` auto-locks, `update` bumps, `install` (no args) respects lock. The baseline user mental model.

**Emerging**: pixi (conda-forge + PyPI polyglot, prefix.dev) — "multi-language toolchain in one manifest" directly overlapping with OCX's `ocx.toml` project-toolchain feature. Worth watching.

**Declining**: pip-tools (no `add`/`remove` verbs, manual workflow) — being displaced by uv. Gradle's lock model (flag-based, non-ergonomic).

---

## 6. OCX-Specific Recommendations

### 6.1 Default-tag policy — adopt `:latest`

**Recommendation: resolve to `:latest` tag when no `:tag` in ocx.toml.**

Every surveyed tool resolves "no version" to "latest." Erroring on missing tag has zero precedent and breaks universal user expectation. Correct behavior: resolve `:latest`, log the resolved tag/digest so users can audit. This is identical to how cargo logs the caret-resolved version in Cargo.lock.

### 6.2 Keep both `update` + `lock` with distinct semantics

**Recommendation: `ocx update` as primary bump verb; `ocx lock` as optional re-resolve-only verb.**

`update` is the correct primary verb — the user's intuition is validated by 6+ tools. Proposed behavioral contract (the poetry model):

- `ocx update [<tool>…]` — resolve newer tags within constraints → write `ocx.lock` → optionally reinstall. `update` (no args) = bump all; `update cmake` = selective bump.
- `ocx lock` — re-resolve all `ocx.toml` entries → write `ocx.lock` only, no install side-effects. Used in CI pre-flight or after manual manifest edits.

**Critical anti-pattern to avoid**: if `ocx lock` and `ocx update` do the same thing, delete one. Two commands with the same behavior and different names is worse than one command with a slightly non-obvious name.

If OCX does not separate "resolve" from "install" (tools always installed on pull), then a standalone `ocx lock` is unnecessary and `ocx update` alone suffices (the npm/pnpm model).

### 6.3 `add`/`remove` lockfile mutation — full rewrite always

**Always write a complete fresh `ocx.lock` on every `add` and `remove`.**

No tool in the survey does partial lockfile patching. Reasons: transitive graph changes on any add/remove; users expect a consistent lockfile snapshot; CI `--frozen` checks rely on complete state. Resolution speed optimization is internal; observable output must always be a complete lockfile.

---

## 7. Sources

- [cargo add — The Cargo Book](https://doc.rust-lang.org/cargo/commands/cargo-add.html)
- [cargo update — The Cargo Book](https://doc.rust-lang.org/cargo/commands/cargo-update.html)
- [cargo generate-lockfile](https://doc.rust-lang.org/cargo/commands/cargo-generate-lockfile.html)
- [Specifying Dependencies — The Cargo Book](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html)
- [npm install — npm Docs](https://docs.npmjs.com/cli/v10/commands/npm-install)
- [npm update — npm Docs](https://docs.npmjs.com/cli/v10/commands/npm-update)
- [yarn add — Yarn Docs](https://yarnpkg.com/cli/add)
- [pnpm install](https://pnpm.io/cli/install)
- [bun add — Bun Docs](https://bun.sh/docs/cli/add)
- [uv add — uv CLI Reference](https://docs.astral.sh/uv/reference/cli/#uv-add)
- [uv lock — uv CLI Reference](https://docs.astral.sh/uv/reference/cli/#uv-lock)
- [poetry CLI — Python Poetry Docs](https://python-poetry.org/docs/cli/)
- [go mod reference](https://go.dev/ref/mod#go-mod-tidy)
- [Gradle Dependency Locking](https://docs.gradle.org/current/userguide/dependency_locking.html)
- [pixi CLI Reference](https://pixi.prefix.dev/v0.29.0/reference/cli/)
- [mise use — mise Docs](https://mise.jdx.dev/cli/use.html)
- [pip-tools documentation](https://pip-tools.readthedocs.io/en/latest/)
