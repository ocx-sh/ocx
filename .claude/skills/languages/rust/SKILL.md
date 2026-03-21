---
name: rust
description: Rust best practices for the OCX project. Covers ownership, error handling, async/tokio patterns, and OCX-specific conventions. References rust-quality.md for comprehensive quality rules.
---

# Rust Development (OCX)

## Quality Reference

Read `.claude/rules/rust-quality.md` for the comprehensive quality guide covering: design patterns, anti-patterns (tiered severity), SOLID/DRY/YAGNI in Rust, async patterns, project pattern consistency, reusability assessment, and code review checklist.

## OCX Error Handling

```rust
// Three-layer model: PackageErrorKind → PackageError → Error

// Single-item task: return kind directly
fn find(&self, pkg: &Identifier) -> Result<InstallInfo, PackageErrorKind> {
    // ...
    Err(PackageErrorKind::NotFound)
}

// Batch: collect errors into command-level Error
fn find_all(&self, pkgs: Vec<Identifier>) -> Result<Vec<InstallInfo>, Error> {
    // Parallel via JoinSet, preserve input order
}

// Library errors: use crate::Error with From impls for ? conversion
// Never unwrap() in library code — always propagate via ?
```

## OCX Async Patterns

```rust
// Parallel _all method — preserve input order
async fn task_all(&self, packages: Vec<Identifier>) -> Result<Vec<Info>, Error> {
    let mut set = JoinSet::new();
    for (index, pkg) in packages.iter().enumerate() {
        let pkg = pkg.clone();
        set.spawn(async move { (index, self.task(&pkg).await) });
    }

    let mut results = Vec::new();
    while let Some(res) = set.join_next().await {
        let (index, result) = res?;
        results.push((index, result));
    }
    results.sort_by_key(|(i, _)| *i);
    // ... collect and handle errors
}

// Progress via tracing (not custom progress bars)
let span = tracing::info_span!("Installing", package = %pkg);
set.spawn(async move { task().await }.instrument(span));

// Sequential with entered guard
for pkg in packages {
    let _guard = tracing::info_span!("Deselecting", package = %pkg).entered();
    // ...
}

// spawn_blocking for CPU-bound work
let compressed = tokio::task::spawn_blocking(move || {
    compress_data(&data)
}).await?;
```

## Tooling

```bash
cargo check                    # Fast check
cargo fmt                      # Format (max_width=120)
cargo clippy --workspace       # Lint
cargo nextest run --workspace  # All tests

# Code duplication detection
duplo crates/ocx_lib/src/ crates/ocx_cli/src/
```

## Key Rules

- **No `.unwrap()` in library code** — always `?` or `.expect("documented reason")`
- **No blocking I/O in async** — use `tokio::fs::*`, `tokio::time::sleep`, `spawn_blocking`
- **No `MutexGuard` across `.await`** — extract data, drop guard, then await
- **JoinSet preserves input order** — spawn with index, sort results by index
- **`ReferenceManager` for symlinks** — never raw `symlink::update/create`
- **`Printable` trait** — single `print_table()` call, static headers, typed enum statuses
- **Bounded channels** — never `mpsc::unbounded_channel()` without explicit justification
