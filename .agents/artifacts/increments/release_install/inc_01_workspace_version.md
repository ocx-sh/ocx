# Increment 01: Workspace Version + Version Guard Test + Shared Version Utility

**Status**: Done
**Completed**: 2026-03-12
**ADR Phase**: 1 (steps 1-3)
**Depends on**: Nothing (first increment)

---

## Goal

Establish the version source of truth in `Cargo.toml` and ensure it's correctly propagated through the build. Extract a shared version utility to avoid duplicated `env!()` calls.

## Tasks

### 1. Add version to workspace Cargo.toml

In `/Cargo.toml`, add `version` to `[workspace.package]`:

```toml
[workspace.package]
version = "0.1.0"
```

### 2. Add `version.workspace = true` to both crates

- `crates/ocx_cli/Cargo.toml`: add `version.workspace = true` under `[package]`
- `crates/ocx_lib/Cargo.toml`: add `version.workspace = true` under `[package]`

If either crate already has a `version` field, replace it. If not, add it.

### 3. Add version guard test

In `crates/ocx_cli/src/` (appropriate test module or new `tests/version.rs`):

```rust
#[test]
fn version_is_set() {
    let version = env!("CARGO_PKG_VERSION");
    assert!(!version.is_empty(), "CARGO_PKG_VERSION must not be empty");
    assert_ne!(version, "0.0.0", "workspace version must be set in Cargo.toml");
}
```

This catches the most common failure mode: forgetting to set the workspace version.

### 4. Extract shared version utility

Currently `version.rs` and `info.rs` both call `env!("CARGO_PKG_VERSION")` directly. Extract to a shared function so the future update check (Increment 16) can reuse it.

Create a function (e.g., in `crates/ocx_cli/src/app/` or a new `version` module):

```rust
/// Returns the compiled OCX version string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

Update `command/version.rs` and `command/info.rs` to call this function instead of `env!()` directly.

## Verification

- [ ] `cargo check` passes
- [ ] `cargo build` passes
- [ ] `cargo nextest run --workspace` passes (including the new version guard test)
- [ ] `cargo run --bin ocx -- version` prints `0.1.0`
- [ ] `cargo run --bin ocx -- info` includes `0.1.0`
- [ ] `task verify` passes

## Files Changed

- `Cargo.toml` (workspace version)
- `crates/ocx_cli/Cargo.toml` (version.workspace = true)
- `crates/ocx_lib/Cargo.toml` (version.workspace = true)
- `crates/ocx_cli/src/app/` or similar (shared version function)
- `crates/ocx_cli/src/command/version.rs` (use shared function)
- `crates/ocx_cli/src/command/info.rs` (use shared function)
- New test file or inline test for version guard
