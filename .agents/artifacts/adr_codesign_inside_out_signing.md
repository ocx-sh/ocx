# ADR: Replace `codesign --deep` with Recursive Inside-Out Signing

## Metadata

**Status:** Superseded
**Date:** 2026-03-06
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** security
**Supersedes:** N/A
**Superseded By:** `adr_codesign_per_file_signing.md`

## Context

ocx automatically ad-hoc signs Mach-O binaries after extracting packages on macOS, since unsigned binaries are terminated immediately on Apple Silicon (`Killed: 9`) and blocked by Gatekeeper on Intel.

The previous implementation used a flat directory walk to collect all sign targets, then signed standalone Mach-O binaries individually and `.app`/`.framework` bundles with `codesign --sign - --force --deep`. This had three problems:

1. **`--deep` is deprecated since macOS 13.0 (Ventura).** Apple recommends signing each component individually in inside-out order. `--deep` has poorly-specified ordering guarantees for nested frameworks — particularly relevant for packages like CMake that embed Qt frameworks with multi-level symlink structures (`Versions/Current → 5`).

2. **No inode deduplication.** Packages may contain hardlinked binaries (same inode, multiple paths). The previous implementation signed each path independently, wasting time on redundant work.

3. **No retry on codesign failure.** A known Apple bug causes `codesign` to fail on certain inodes. Homebrew works around this by copying the file to a new inode, signing, and moving it back. ocx had no such fallback.

## Decision Drivers

- Apple deprecated `--deep` and recommends inside-out per-component signing
- Homebrew — the de facto standard for macOS binary management — uses per-file signing without `--deep`
- Packages with nested framework bundles (e.g., CMake with Qt) require correct signing order
- Hardlinked binaries in packages cause redundant signing work
- Known Apple `codesign` bug requires a copy-to-new-inode workaround

## Considered Options

### Option 1: Keep `--deep`, add inode dedup

**Description:** Keep the current two-pass approach (standalone binaries + `--deep` for bundles), but add inode tracking to skip hardlinked duplicates.

| Pros | Cons |
|------|------|
| Minimal code change | `--deep` is deprecated and may be removed |
| Known working approach | No control over signing order within bundles |
| | Does not address the Apple inode bug |

### Option 2: Flat walk with topological sort

**Description:** Collect all sign targets in a flat list, sort by path depth (deepest first), sign individually without `--deep`.

| Pros | Cons |
|------|------|
| Inside-out order achieved via sort | Requires separate bundle detection pass |
| Single flat list, easy to reason about | Sort-based ordering is fragile for edge cases |
| | Two-pass design (collect, then sign) adds complexity |

### Option 3: Recursive inside-out with inode dedup

**Description:** Replace the flat walk with a recursive function that naturally enforces inside-out order: recurse into subdirectories first, sign files in current directory, then seal the bundle if applicable. Track signed inodes via a shared `HashSet<u64>` to skip hardlinked duplicates. Retry failed signings with a copy-to-new-inode workaround.

| Pros | Cons |
|------|------|
| Inside-out order is structural, not sort-based | Recursive async requires `Box::pin` |
| Inode dedup prevents redundant work | Mutex on inode set (low contention in practice) |
| Matches Homebrew's per-file approach | |
| Retry workaround handles known Apple bug | |
| Symlinks naturally skipped (`file_type()` doesn't follow) | |

## Decision Outcome

**Chosen Option:** Option 3 — Recursive inside-out with inode dedup

**Rationale:** The recursive approach makes inside-out ordering a structural guarantee rather than relying on sort order. It naturally handles arbitrary nesting depth and matches Homebrew's proven approach. The inode deduplication and retry-with-copy workaround address real-world edge cases that the previous implementation ignored.

### Consequences

**Positive:**
- Correct signing order for arbitrarily nested bundles (frameworks inside `.app` bundles)
- No dependency on deprecated `--deep` flag
- Hardlinked binaries signed only once
- Resilient to known Apple `codesign` inode bug
- Symlinks are never followed — `DirEntry::file_type()` returns `is_symlink()` on Unix, so symlink entries are neither recursed into nor signed
- Aligns with Homebrew's approach, the de facto standard

**Negative:**
- Recursive async function requires `Box::pin` for the recursive call
- Slightly more code than the flat walk approach

**Risks:**
- `Mutex<HashSet<u64>>` could theoretically contend under high parallelism, but in practice signing is I/O-bound and directory-level parallelism is limited. Mitigated by checking `is_macho` before acquiring the lock.

## Technical Details

### Architecture

```
sign_directory(content/)
├── sign_directory(App.app/)              ← recurse first
│   ├── sign_directory(Contents/)
│   │   ├── sign_directory(Frameworks/)
│   │   │   └── sign_directory(Qt.framework/)
│   │   │       └── sign_directory(Versions/)
│   │   │           ├── sign_directory(5/)
│   │   │           │   └── sign_binary(QtCore)     ← inode recorded
│   │   │           └── (Current → 5)               ← symlink, skipped
│   │   │       then: sign_bundle(Qt.framework)     ← bundle sealed
│   │   └── sign_directory(MacOS/)
│   │       └── sign_binary(app)                    ← inode recorded
│   then: sign_bundle(App.app)                      ← .app sealed LAST
├── sign_binary(standalone-tool)                    ← standalone binary
```

### Key Functions

```rust
/// Recursive inside-out signing. Symlinks not followed.
async fn sign_directory(path: &Path, signed_inodes: &Mutex<HashSet<u64>>)

/// Signs a Mach-O binary with --preserve-metadata. Retries via copy on failure.
async fn sign_binary(path: &Path)

/// Signs a bundle (.app/.framework) without --deep, after contents are signed.
async fn sign_bundle(path: &Path)

/// Returns true for .app and .framework extensions.
fn is_bundle(path: &Path) -> bool

/// Returns the inode number (unix only, None on other platforms).
async fn file_inode(path: &Path) -> Option<u64>

/// Runs codesign, returns true on success.
async fn try_codesign(args: &[&str], path: &Path) -> bool

/// Copy to temp (new inode) → sign → move back. Apple codesign bug workaround.
async fn retry_sign_with_copy(args: &[&str], path: &Path) -> std::io::Result<()>
```

### Signing Commands

Individual Mach-O binaries:
```sh
codesign --sign - --force --preserve-metadata=entitlements,requirements,flags,runtime <binary>
```

Bundle sealing (after contents are signed):
```sh
codesign --sign - --force <bundle>
```

## Implementation Plan

1. [x] Replace flat walk with recursive `sign_directory`
2. [x] Add inode deduplication via `Mutex<HashSet<u64>>`
3. [x] Add retry-with-copy fallback for Apple codesign bug
4. [x] Remove `--deep` from bundle signing
5. [x] Update tests (22 tests covering all paths)
6. [x] Update FAQ documentation to reflect new approach

## Validation

- [x] All 130 workspace tests pass (`cargo nextest run --workspace`)
- [x] Clippy clean (`cargo clippy --workspace`)
- [x] FAQ documentation updated — no `--deep` references remain

## Links

- [PR #3: Replace codesign --deep with recursive inside-out signing](https://github.com/ocx-sh/ocx/pull/3)
- [Apple codesign man page](https://keith.github.io/xcode-man-pages/codesign.1.html)
- [Homebrew codesign implementation](https://github.com/Homebrew/brew/blob/master/Library/Homebrew/extend/os/mac/keg.rb)
- [MADR format](https://adr.github.io/madr/)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-06 | mherwig | Accepted — implemented in PR #3 |
