# Increment 16: Update Check Notification + OCX_NO_UPDATE_CHECK

**Status**: Not Started
**Completed**: —
**ADR Phase**: 8 (steps 23-24)
**Depends on**: Increment 01 (shared version utility)

---

## Goal

Add a non-intrusive update notification that checks the local index for a newer OCX version on CLI startup, and introduce the `OCX_NO_UPDATE_CHECK` environment variable.

## Tasks

### 1. Implement update check function

In `crates/ocx_cli/src/` (e.g., in `app/` or a new `update_check` module):

```rust
/// Check local index for a newer OCX version and print a notice to stderr.
/// Returns immediately if disabled or no update available.
pub async fn check_for_update(ctx: &Context) {
    // Disabled conditions (checked in order):
    // 1. OCX_NO_UPDATE_CHECK env var is truthy
    // 2. CI=true env var
    // 3. OCX_OFFLINE=true env var
    // 4. stderr is not a terminal

    let current = Version::parse(version());  // shared version utility from Inc 01

    // Query local index for ocx tags (zero network, instant)
    let ocx_id = Identifier::parse("ocx.sh/ocx").unwrap();
    let tags = ctx.default_index().list_tags(&ocx_id).await;

    if let Ok(Some(tags)) = tags {
        if let Some(latest) = find_latest_semver(&tags) {
            if latest > current {
                eprintln!(
                    "ocx {} available (current: {}). Run: ocx install ocx:{} --select",
                    latest, current, latest
                );
            }
        }
    }
}
```

### 2. Disable conditions

Check these in order (early return if any is true):

| Condition | Detection | Rationale |
|---|---|---|
| `OCX_NO_UPDATE_CHECK=true` | Env var (truthy: `1`, `true`, `yes`) | Explicit user opt-out |
| `CI=true` | Env var | Standard CI detection |
| `OCX_OFFLINE=true` | Env var | Index may be stale |
| stderr is not a terminal | `!std::io::stderr().is_terminal()` | Don't inject noise into piped stderr |

Use the same truthy-value parsing as `OCX_OFFLINE` and `OCX_REMOTE`.

### 3. Wire into CLI startup

Call `check_for_update()` in the CLI main function, after `Context` is built but before command dispatch. The check should:
- Be async but non-blocking for the main command (could be a spawned task that prints after command completes, or just run inline since it's instant — local index only)
- Only print to stderr (never stdout)
- Never fail the command — all errors are silently swallowed

### 4. Add `OCX_NO_UPDATE_CHECK` to documentation

Update three places:
- `CLAUDE.md` environment variable table
- `website/src/docs/reference/environment.md`
- `.claude/rules/cli-commands.md` (if env vars are documented there)

Also add `OCX_NO_MODIFY_PATH` to the same documentation (from the install scripts):

| Variable | Purpose | Default |
|---|---|---|
| `OCX_NO_UPDATE_CHECK` | Disable update check notification | `false` |
| `OCX_NO_MODIFY_PATH` | Disable shell profile modification during install | `false` |

### 5. Add unit tests

Test the update check logic:
- Version comparison: current < latest → prints notice
- Version comparison: current >= latest → prints nothing
- Disabled when env vars are set
- Empty/missing index → no error, no output

### 6. Add acceptance test

Add a pytest test that verifies the update check behavior:
- Install an older version, populate index with newer version, run a command, check stderr for the notice
- Or: verify that `OCX_NO_UPDATE_CHECK=1` suppresses the notice

## Verification

- [ ] Update check prints to stderr when a newer version is in the local index
- [ ] Update check is silent when current version is latest
- [ ] `OCX_NO_UPDATE_CHECK=1` suppresses the notice
- [ ] `CI=true` suppresses the notice
- [ ] `OCX_OFFLINE=true` suppresses the notice
- [ ] Non-terminal stderr suppresses the notice
- [ ] Errors in index query are silently swallowed
- [ ] `CLAUDE.md` env var table updated
- [ ] `reference/environment.md` updated with both new env vars
- [ ] Unit tests pass
- [ ] `task verify` passes

## Files Changed

- `crates/ocx_cli/src/app/` or new module (update check implementation)
- `crates/ocx_cli/src/main.rs` (wire into startup)
- `CLAUDE.md` (env var table)
- `website/src/docs/reference/environment.md` (env var docs)
- Test files (unit + acceptance)

## Notes

- The key insight: this checks the **local** index only. Zero network cost. The index is already on disk from previous `ocx index update` calls.
- The shared version function from Increment 01 avoids duplicating `env!("CARGO_PKG_VERSION")`.
- The notice goes to stderr so it doesn't pollute stdout (safe for `ocx env` piped into `eval`).
- semver parsing: use the `semver` crate (already likely a dependency via cargo).
