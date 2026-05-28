# Task Completion

When a coding task is considered done, run these in order. Subsystem gates first for fast feedback during the review-fix loop; full `task verify` once before the final commit.

## Mandatory final gate (before commit)

```sh
cargo fmt              # always — formatting must be clean
task verify            # parallel lint, then build + unit tests (cached)
```

If anything in the gate auto-fixed files (formatter, clippy --fix, license headers), re-stage and re-run.

## Subsystem fast gates (during dev loop — match scope of change)

| Scope                                       | Gate                              |
|---------------------------------------------|-----------------------------------|
| Rust only (`crates/**/*.rs`, `Cargo.toml`)  | `task rust:verify`                |
| Shell scripts (`*.sh`, `*.bash`)            | `task shell:verify`               |
| `.claude/**` (AI config, rules, skills)     | `task claude:verify`              |
| Website (`website/**`)                      | `task website:build`              |
| Acceptance tests (`test/**`)                | `task test:quick` or `task test`  |
| GitHub workflows (`.github/workflows/**`)   | `actionlint` (via task wrappers)  |

`task rust:verify` runs sequentially fail-fast: `format:check → clippy:check → license:check → license:deps → build → test:unit`. Trust it over mid-edit rust-analyzer snapshots.

## When verification is partial

- After Rust edits: `cargo check` (fast) → `task rust:verify` (full).
- After acceptance-test edits: `task test:quick` (skips rebuild).
- After AI config edits: `task claude:tests` then `task claude:verify`.
- After dep changes (`Cargo.toml`, `deny.toml`, `.licenserc.toml`): full `task verify`, plus `task --force verify` if caching might mask a stale result.

## Bypass caching when needed

```sh
task --force verify    # full gate, ignore cached statuses
```

## Coverage (optional, on request)

```sh
task coverage          # generate LLVM report
task coverage:open     # open HTML report
```

## Commit hygiene (every task)

- Conventional Commits message (`feat:`, `fix:`, `refactor:`, `ci:`, `chore:`); no `Co-Authored-By`.
- Branch only — never on `main`; never push (human decides).
- Bug fix → regression test included.
- Touched user-facing surfaces? Update website docs / ADR alongside the code change (plan enumerates these).
- If `task verify` modifies tracked files (fmt, clippy --fix, license), commit those changes (or amend) before declaring done.
