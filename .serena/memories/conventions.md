# Conventions

## Design principles (canonical: `.claude/rules/quality-core.md`)

SOLID, DRY (extract only when 2+ genuinely different callers), KISS, Boring Tech (≈3 innovation tokens total), YAGNI. **Two Hats**: never mix refactoring and optimization in one commit.

## Anti-pattern severity

- **Block**: hardcoded secrets, unvalidated external input at boundaries, silent error swallowing, god objects (15+ fields/methods crossing concerns).
- **Warn**: boolean params where enum clearer, stringly-typed APIs, clones in hot paths, missing error context.
- **Suggest**: nice-to-haves.

## Verification honesty

Banned in commits/reviews/completion reports: "should work", "probably", "likely", "seems to", "Great!", "Perfect!", "Done!". Replace with cited evidence ("verified by `<test>`" / "ran `task rust:verify`, passed"). Hedging = Warn-tier; "verified" without citation = Block-tier (false verification).

## Comments

Explain *why*, never *what*. No comment that restates code. Reader = senior engineer.

## Rust specifics (`.claude/rules/quality-rust.md`, `quality-rust-errors.md`, `quality-rust-exit_codes.md`)

- Edition 2024. Max line width 120 (`rustfmt.toml`).
- Error types: don't over-engineer variants — distinguish caller-actionable cases only.
- CLI exit codes: see `quality-rust-exit_codes.md` for the canonical mapping.
- Performance checklist: no blocking `std::fs::*` in async, no `MutexGuard` across `.await`, no unbounded channels without backpressure.

## CLI design

- Flags before positional args.
- No per-subcommand `--format` flag — format is a parent/context flag.
- Substance in `ocx_lib`; `ocx_cli` is a thin clap shell that calls into the lib.
- Generalize commands over `Identifier` so they work for both package and project refs.
- Extend existing pipelines via override hooks rather than parallel pipelines.

## Workflow gates

- Classify every task via `.claude/rules/workflow-intent.md` (Bug/Feature/Refactor) before starting.
- Scan GitHub for related issues/PRs before planning. Track work in GitHub issues, not local notes.
- Always preview GitHub issues/PRs before creating (proposal-first).
- Feature plans must enumerate every doc/website/ADR surface they touch.
- Plans use phase-ordered TDD + subagent delegation via `/swarm-plan`; single linear plans only for narrow fixes.

## Git workflow

- Conventional Commits required. `chore:` for AI config, skills, CLAUDE.md, tooling.
- No `Co-Authored-By` trailers.
- Two-phase model: `/commit` during dev (or `task checkpoint`), `/finalize` before landing.
- Branch only — never commit on `main`; never push (human decides).
- One subagent per rebase conflict; never resolve inline. Audit each rebased commit's stat against scope.

## AI config (`.claude/`)

- `.claude/rules.md` is the catalog; every rule add/remove/rescope must update it in the same commit.
- Path-scoped rules auto-load on matching edits; non-path-scoped rules belong in catalog "By concern".
- Skills live in `.claude/skills/`; personas: `/architect`, `/builder`, `/qa-engineer`, `/security-auditor`, `/code-check`, `/swarm-plan`, `/swarm-execute`, `/swarm-review`.
- Runtime state in `.claude/state/`, regenerable bulk in `cache/`.
- Structural tests in `.claude/tests/test_ai_config.py` enforce catalog freshness — run `task claude:tests` (or `task claude:verify`) before committing config changes.

## Refactoring

Prefer semantic tooling (LSP `findReferences`, `workspaceSymbol`, `goToDefinition` — exposed via Serena) over Grep for symbol-level ops. Grep only for non-code (comments, docs, config).

## Tests & docs (`.claude/rules/subsystem-tests.md`, `docs-style.md`, `subsystem-website.md`)

- Acceptance tests in `test/` use pytest + Docker Compose `registry:2` on `localhost:5000`. Honour fixture patterns documented in the subsystem rule.
- DAMP over DRY in test code — readability beats dedup.
- User guides are use-case-driven (start from user goals, link internals to "In Depth"); no migration prose for pre-1.0 changes.

## Mirror / packaging quirks

- `LayerRef::Digest` absent locally → auto-fetch from registry; never error.
- Don't WARN on common benign states (e.g. departed project ref) — self-heal on next clean.
- Cache coherence: prefer `default_index` over direct `context.remote_client()`.
