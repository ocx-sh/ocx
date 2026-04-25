# OCX Rule Catalog

Entry point for `.claude/rules/`. Path-scoped rules fire only on file edit match — not enough during planning, research, or architecture before code exists.

## When to consult this catalog

- Plan/architecture phase — scan "By concern" before drafting
- Research phase — find quality criteria for topic
- Onboarding new area — scan "By subsystem" / "By language"
- Writing ADRs, RFCs, research artifacts — find relevant rules
- Updating AI config — see "AI config changes" row in "By concern"

Editing code + relevant rule auto-loaded via `paths:` glob → no need re-read from catalog. Catalog for cases where path-scoping cannot fire.

## How to update this catalog

Any change to `.claude/rules/` must reflect here in same commit:

- New rule → add row in relevant tables below
- Deleted rule → remove all references
- Changed `paths:` glob → update "By auto-load path" table
- New skill → add entry in "Skills by task topic"

Structural tests in `.claude/tests/test_ai_config.py` fail when catalog drifts from reality. Run `task claude:tests` locally before committing catalog changes.

## By concern — "if you care about X, consult these"

| Concern | Rules & skills |
|---|---|
| Product vision / positioning / competitors | [product-context.md](./rules/product-context.md) — canonical identity doc; update when positioning shifts (see Update Protocol section at bottom) |
| Adopting a third-party library or tool | [product-tech-strategy.md](./rules/product-tech-strategy.md), [subsystem-deps.md](./rules/subsystem-deps.md), skill `deps`, `quality-{lang}.md` |
| Starting any task (work-type routing) | [workflow-intent.md](./rules/workflow-intent.md) (global) — classifies work, checks GitHub context, routes to correct workflow |
| Designing a new feature | [arch-principles.md](./rules/arch-principles.md), [workflow-feature.md](./rules/workflow-feature.md), `subsystem-{target}.md`, skill `architect` |
| Fixing a bug | [workflow-bugfix.md](./rules/workflow-bugfix.md) — Reproduce → RCA → Regression Test → Fix → Verify |
| Refactoring code | [workflow-refactor.md](./rules/workflow-refactor.md) — Safety Net → Scope → Transform → Verify → Repeat |
| Website / docs work | [quality-typescript.md](./rules/quality-typescript.md), [quality-vite.md](./rules/quality-vite.md), [subsystem-website.md](./rules/subsystem-website.md), [docs-style.md](./rules/docs-style.md), skill `docs` |
| Security-sensitive change | [quality-security.md](./rules/quality-security.md), [subsystem-ci.md](./rules/subsystem-ci.md), skill `security-auditor` |
| CLI command changes | [subsystem-cli.md](./rules/subsystem-cli.md), [subsystem-cli-api.md](./rules/subsystem-cli-api.md), [subsystem-cli-commands.md](./rules/subsystem-cli-commands.md) |
| Writing tests | [subsystem-tests.md](./rules/subsystem-tests.md), [quality-python.md](./rules/quality-python.md), [quality-rust.md](./rules/quality-rust.md), skill `qa-engineer` |
| Metadata / schema changes | [subsystem-metadata-schema.md](./rules/subsystem-metadata-schema.md), [subsystem-package.md](./rules/subsystem-package.md) |
| CI / workflows | [subsystem-ci.md](./rules/subsystem-ci.md), [workflow-release.md](./rules/workflow-release.md) |
| AI config changes | [meta-ai-config.md](./rules/meta-ai-config.md) + this catalog, skill `meta-maintain-config` |
| GitHub issues & PRs / planning artifacts | [workflow-github.md](./rules/workflow-github.md), [workflow-feature.md](./rules/workflow-feature.md) |
| Commits, branches, rebasing, landing on main | [workflow-git.md](./rules/workflow-git.md), skills `commit`, `finalize` |
| Swarm / multi-agent workflows | [workflow-swarm.md](./rules/workflow-swarm.md), [workflow-feature.md](./rules/workflow-feature.md), skills `swarm-plan`, `swarm-execute`, `swarm-review` |
| Code quality audit | [quality-core.md](./rules/quality-core.md), `quality-{lang}.md`, skill `code-check` |
| Error type design (Rust) | [quality-rust.md](./rules/quality-rust.md), [quality-rust-errors.md](./rules/quality-rust-errors.md) |
| CLI exit code design (Rust) | [quality-rust.md](./rules/quality-rust.md), [quality-rust-exit_codes.md](./rules/quality-rust-exit_codes.md) |
| Mirror / bundling | [subsystem-mirror.md](./rules/subsystem-mirror.md) |
| Taskfiles / build pipeline / caching | [subsystem-taskfiles.md](./rules/subsystem-taskfiles.md) |
| Releases | [workflow-release.md](./rules/workflow-release.md) |

## By language

| Language | Quality rule | Related |
|---|---|---|
| Rust | [quality-rust.md](./rules/quality-rust.md) | [quality-rust-errors.md](./rules/quality-rust-errors.md), [quality-rust-exit_codes.md](./rules/quality-rust-exit_codes.md), [arch-principles.md](./rules/arch-principles.md), [quality-core.md](./rules/quality-core.md), [subsystem-deps.md](./rules/subsystem-deps.md) |
| Python (acceptance tests) | [quality-python.md](./rules/quality-python.md) | [subsystem-tests.md](./rules/subsystem-tests.md) |
| TypeScript (website) | [quality-typescript.md](./rules/quality-typescript.md) | [quality-vite.md](./rules/quality-vite.md), [subsystem-website.md](./rules/subsystem-website.md) |
| Bash (tasks, hooks) | [quality-bash.md](./rules/quality-bash.md) | — |
| Vue / VitePress | [quality-vite.md](./rules/quality-vite.md) | [subsystem-website.md](./rules/subsystem-website.md), [docs-style.md](./rules/docs-style.md) |

## By subsystem

Mirrors subsystem table in `CLAUDE.md`. Catalog = single source of truth — `CLAUDE.md` may summarize or reference by pointer.

| Subsystem | Rule | Path scope |
|---|---|---|
| OCI registry/index | [subsystem-oci.md](./rules/subsystem-oci.md) | `crates/ocx_lib/src/oci/**` |
| Storage/symlinks | [subsystem-file-structure.md](./rules/subsystem-file-structure.md) | `crates/ocx_lib/src/file_structure/**` |
| Package metadata | [subsystem-package.md](./rules/subsystem-package.md) | `crates/ocx_lib/src/package/**` |
| Package manager | [subsystem-package-manager.md](./rules/subsystem-package-manager.md) | `crates/ocx_lib/src/package_manager/**` |
| CLI commands/API | [subsystem-cli.md](./rules/subsystem-cli.md) | `crates/ocx_cli/src/**` |
| Mirror tool | [subsystem-mirror.md](./rules/subsystem-mirror.md) | `crates/ocx_mirror/**` |
| Acceptance tests | [subsystem-tests.md](./rules/subsystem-tests.md) | `test/**` |
| Website/docs | [subsystem-website.md](./rules/subsystem-website.md) | `website/**` |
| CI / workflows | [subsystem-ci.md](./rules/subsystem-ci.md) | `.github/workflows/**` |
| Dependencies | [subsystem-deps.md](./rules/subsystem-deps.md) | `Cargo.toml`, `deny.toml`, `.licenserc.toml` |
| Taskfiles | [subsystem-taskfiles.md](./rules/subsystem-taskfiles.md) | `taskfile.yml`, `taskfiles/**/*.yml`, `**/taskfile.yml` |

## By auto-load path — "what fires when you edit"

| Edit path | Rules that auto-load |
|---|---|
| `**/*.rs` | [quality-rust.md](./rules/quality-rust.md), [quality-rust-errors.md](./rules/quality-rust-errors.md), [quality-rust-exit_codes.md](./rules/quality-rust-exit_codes.md) (+ [arch-principles.md](./rules/arch-principles.md) under `crates/**`, `external/**`) |
| `**/Cargo.toml`, `**/Cargo.lock` | [quality-rust.md](./rules/quality-rust.md) |
| `Cargo.toml`, `crates/*/Cargo.toml`, `deny.toml`, `.licenserc.toml` | [subsystem-deps.md](./rules/subsystem-deps.md) |
| `crates/ocx_lib/src/oci/**` | + [subsystem-oci.md](./rules/subsystem-oci.md) |
| `crates/ocx_lib/src/file_structure/**`, `file_structure.rs`, `reference_manager.rs`, `symlink.rs` | + [subsystem-file-structure.md](./rules/subsystem-file-structure.md) |
| `crates/ocx_lib/src/package/**`, `package.rs` | + [subsystem-package.md](./rules/subsystem-package.md) |
| `crates/ocx_lib/src/package_manager/**`, `package_manager.rs` | + [subsystem-package-manager.md](./rules/subsystem-package-manager.md) |
| `crates/ocx_lib/src/package/metadata/**`, `crates/ocx_schema/**` | + [subsystem-metadata-schema.md](./rules/subsystem-metadata-schema.md) |
| `crates/ocx_cli/src/**` | + [subsystem-cli.md](./rules/subsystem-cli.md) |
| `crates/ocx_cli/src/api/**`, `command/**` | + [subsystem-cli-api.md](./rules/subsystem-cli-api.md), [subsystem-cli-commands.md](./rules/subsystem-cli-commands.md) |
| `crates/ocx_mirror/**`, `mirrors/**` | + [subsystem-mirror.md](./rules/subsystem-mirror.md) |
| `test/**` | [subsystem-tests.md](./rules/subsystem-tests.md) |
| `test/**/*.py`, `**/*.py` | + [quality-python.md](./rules/quality-python.md) |
| `website/**` | [quality-typescript.md](./rules/quality-typescript.md), [quality-vite.md](./rules/quality-vite.md), [subsystem-website.md](./rules/subsystem-website.md), [docs-style.md](./rules/docs-style.md), [product-context.md](./rules/product-context.md) |
| `**/*.ts`, `**/*.tsx`, `**/tsconfig*.json` | [quality-typescript.md](./rules/quality-typescript.md) |
| `**/vite.config.*`, `**/.vitepress/config.*` | [quality-vite.md](./rules/quality-vite.md) |
| `**/*.sh`, `**/*.bash` | [quality-bash.md](./rules/quality-bash.md) |
| `.github/workflows/**`, `.github/actions/**`, `dependabot.yml` | [subsystem-ci.md](./rules/subsystem-ci.md), [quality-security.md](./rules/quality-security.md) |
| `.github/ISSUE_TEMPLATE/**` | [workflow-github.md](./rules/workflow-github.md) |
| `dist-workspace.toml`, `cliff.toml`, `CHANGELOG.md`, release workflows | [workflow-release.md](./rules/workflow-release.md), [workflow-git.md](./rules/workflow-git.md) |
| `taskfile.yml`, `taskfiles/**/*.yml`, `**/taskfile.yml` | [subsystem-taskfiles.md](./rules/subsystem-taskfiles.md) |
| `.claude/**` | [meta-ai-config.md](./rules/meta-ai-config.md) |
| `.claude/agents/**`, `.claude/skills/swarm-*/**` | + [workflow-swarm.md](./rules/workflow-swarm.md), [workflow-feature.md](./rules/workflow-feature.md) |

Globals (always loaded or imported into `CLAUDE.md`): [quality-core.md](./rules/quality-core.md),
[product-tech-strategy.md](./rules/product-tech-strategy.md), [workflow-intent.md](./rules/workflow-intent.md), this catalog.

Scoped workflow rules (loaded by path match, consumed by skills on demand):
[quality-security.md](./rules/quality-security.md) (`.github/workflows/**`, `.github/actions/**`),
[workflow-github.md](./rules/workflow-github.md) (`.github/ISSUE_TEMPLATE/**`),
[workflow-git.md](./rules/workflow-git.md) (`CHANGELOG.md`, `cliff.toml`, `dist-workspace.toml`).

## Declared Path-Scope Overlaps

Every pair of rules sharing `paths:` pattern must be covered by declared group. Undeclared overlaps fail `test_path_overlaps_declared_or_absent`.

Exempt from overlap detection (intended broad coupling):

- Shareable `quality-*.md` — language quality rules co-fire with subsystem rules on language globs.
- `workflow-bugfix.md` / `workflow-refactor.md` — source-work-surface scope per `adr_ai_config_path_scope_correction.md`; co-firing with subsystem rules = intended coupling.

| Declared group | Shared scope |
|---|---|
| `quality-security.md` + `subsystem-ci.md` | `.github/workflows/**`, `.github/actions/**` |
| `workflow-git.md` + `workflow-release.md` | `CHANGELOG.md`, `cliff.toml`, `dist-workspace.toml` |
| `docs-style.md` + `subsystem-website.md` + `product-context.md` | `website/**` |
| `product-context.md` + `workflow-feature.md` | `.claude/artifacts/**` |
| `subsystem-cli-api.md` + `subsystem-cli-commands.md` | `crates/ocx_cli/src/command/**` |
| `workflow-feature.md` + `workflow-swarm.md` | `.claude/agents/**`, `.claude/skills/swarm-*/**` |

## Skills by task topic

| Task topic | Skill |
|---|---|
| Adding/choosing a dependency | `deps` |
| Writing docs | `docs` |
| Security review | `security-auditor` |
| Architecture decision | `architect` |
| Code quality audit | `code-check` |
| Implementation / debugging | `builder` |
| Test strategy | `qa-engineer` |
| Planning a feature (multi-agent) | `swarm-plan` |
| Executing a feature (multi-agent) | `swarm-execute` |
| Adversarial review | `swarm-review` |
| Releases | (see [workflow-release.md](./rules/workflow-release.md)) |
| AI config maintenance | `meta-maintain-config`, `meta-validate-context` |
| Mirror configuration | `ocx-create-mirror` |
| Roadmap sync | `ocx-sync-roadmap` |
| Commits (working phase) | `commit` |
| Finalize branch for merge onto main | `finalize` |
| Suggest next slash command from current state | `next` |