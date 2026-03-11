# ADR: Replace Bundle Signing with Per-File Mach-O Signing

## Metadata

**Status:** Accepted
**Date:** 2026-03-11
**Supersedes:** `adr_codesign_inside_out_signing.md` (the inside-out bundle signing approach)
**Related analysis:** `analysis_codesign_team_id_mismatch.md`

---

## Context

The inside-out recursive signing approach (see `adr_codesign_inside_out_signing.md`) was designed to replace deprecated `--deep` while maintaining proper bundle seals. It introduced `sign_bundle()`, `is_bundle()`, `is_versioned_framework()`, and `is_framework_version_dir()` to detect and seal `.app` and `.framework` bundles.

Despite three rounds of patches, the approach still produces "different Team IDs" dyld errors for cmake-gui loading Qt frameworks. The root cause is that bundle sealing for versioned frameworks is difficult to implement correctly — specifically, the interaction between individual file signing (with `--preserve-metadata=requirements`) and subsequent bundle sealing creates Team ID conflicts. Each fix has added complexity without fully resolving the underlying issue.

**The implementation has become a collection of special cases:**
- Skip top-level `.framework/` files (commit `0cf16fc`)
- Skip files inside `Versions/N/` (session fix #1)
- Remove `requirements` from `--preserve-metadata` (session fix #2)
- Inside-out ordering requirement with `Box::pin` recursive async
- Four different bundle-detection predicates

---

## Core Insight

**macOS at runtime only checks the binary's own embedded signature, not the `_CodeSignature/CodeResources` bundle seal.**

The bundle seal (`_CodeSignature/CodeResources`) is checked by:
1. Gatekeeper — when the user opens an app through Finder / double-click
2. Explicit `codesign --verify --deep`

OCX packages are run via `ocx exec` (direct process invocation), not through Finder. The macOS kernel only needs:
- Each Mach-O being executed has a valid signature
- All binaries in a loading chain have a consistent Team ID (all ad-hoc = Team ID none, or all from the same developer certificate)

**Bundle sealing is unnecessary for OCX's use case.** Making the bundle seals correct is both complex and fragile for third-party packages.

---

## Decision Drivers

- Bundle sealing for third-party packages (Qt frameworks signed by The Qt Company) requires overwriting existing `_CodeSignature/CodeResources` that reference a foreign Team ID — this is the source of all Team ID conflicts
- Homebrew uses per-file signing only and it works reliably for macOS binary distribution
- Apple TN2206 recommends per-component signing, but "component" for our use case is the individual Mach-O binary, not the bundle directory
- OCX packages are run via `ocx exec`, not through Finder; the bundle seal is irrelevant for this execution path
- KISS: per-file signing has zero special cases vs. five with bundle sealing

---

## Considered Options

### Option A: Continue patching the bundle sealing approach (status quo)

Continue adding special cases to handle edge cases in bundle detection and signing order.

| Pros | Cons |
|------|------|
| Produces valid bundle seals for `codesign --verify --deep` | Complexity accumulates with each new package type |
| Useful if packages are ever opened via Finder | Bundle sealing of third-party frameworks has fundamental Team ID conflicts |
| | Every new framework variant (Qt5 vs Qt6, different versioning) may need another fix |
| | Tests cannot verify real signing on Linux CI |

### Option B: Per-file signing only (Homebrew approach)

Walk the content tree, sign every Mach-O regular file. No bundle directory signing.

| Pros | Cons |
|------|------|
| Zero special cases — one rule for everything | `codesign --verify --deep` on a signed `.app` bundle will fail |
| Proven by Homebrew for macOS binary distribution | Opening via Finder may show security warning (stale bundle seal) |
| No Team ID conflicts (no `--preserve-metadata=requirements`) | |
| Handles any package structure automatically | |
| Significantly less code | |

### Option C: Per-file, skip if already ad-hoc signed

Check each binary first; only re-sign if not already ad-hoc signed.

| Pros | Cons |
|------|------|
| Preserves already-correct signatures | Fragile: a binary could be ad-hoc signed with a different Team ID |
| Reduces re-signing for packages that ship pre-signed | Cannot tell if the existing ad-hoc signature is compatible with the rest |
| | Certificate-signed binaries (Qt Team ID) would be kept → still causes Team ID conflicts |

---

## Decision Outcome

**Chosen Option: B — Per-file signing only**

**Rationale:**

1. **Correctness for OCX's use case**: `ocx exec` bypasses Gatekeeper bundle-seal checks entirely. The kernel only verifies individual binary signatures at load time. Per-file signing produces valid individual signatures.

2. **Team ID consistency**: All Mach-O files get a fresh ad-hoc signature (Team ID = none). There is no mechanism for a Team ID conflict when every binary is independently signed to the same identity.

3. **Third-party package robustness**: Packages from any publisher — whether they use `.app` bundles, versioned Qt frameworks, flat dylib directories, or custom layouts — all work with the same one rule: sign every Mach-O file.

4. **Proven approach**: Homebrew has used per-file signing since removing `--deep` support and handles millions of macOS package installations.

5. **Simpler code**: Removes ~100 lines of bundle-detection logic. The signing loop becomes a straightforward recursive Mach-O file walk.

**Trade-off accepted:** `codesign --verify --deep` on a package's `.app` bundle will fail because the bundle seals are stale (original developer seals, binaries re-signed). This is acceptable because:
- OCX users run packages via `ocx exec`, not by opening `.app` bundles in Finder
- For Finder access, the user can set `OCX_DISABLE_CODESIGN=1` and manage signing manually, or the package publisher can ship a properly signed package
- The content-addressed object store is immutable; stale seals are a one-time state, not a security regression

---

## Implementation

### Signing command

```sh
codesign --sign - --force --preserve-metadata=entitlements,flags,runtime <binary>
```

- `--sign -`: ad-hoc signature (no Team ID)
- `--force`: replace any existing signature (required for certificate-signed third-party binaries)
- `--preserve-metadata=entitlements,flags,runtime`: preserve app-declared capabilities, code signing flags (`CS_HARD`, `CS_KILL`, etc.), and hardened runtime — all relevant to correct execution behavior
- `requirements` is intentionally dropped: the original Designated Requirement encodes the issuing certificate's Team ID, which an ad-hoc signature cannot satisfy, causing dyld "different Team IDs" errors for third-party binaries (Qt, etc.)

### What changes

**Remove:**
- `sign_bundle()` — no bundle directory signing
- `is_bundle()` — not needed
- `is_versioned_framework()` — not needed
- `is_framework_version_dir()` — not needed
- The inside-out ordering requirement (no longer matters without bundle sealing)

**Simplify:**
- `sign_directory()` → straightforward recursive file walk; no step 2/3 distinction, no skip conditions

**Keep:**
- `sign_binary()` with `--preserve-metadata=entitlements,flags,runtime`
- `try_codesign()`, `retry_sign_with_copy()` (inode bug workaround)
- `is_macho()`, `file_inode()` (Mach-O detection and hardlink dedup)
- `remove_quarantine()` (still useful)

### Signing walk (pseudocode)

```
for each entry in directory (recursively, symlinks not followed):
    if regular file AND is_macho AND inode not already seen:
        codesign --sign - --force --preserve-metadata=entitlements,flags,runtime <file>
```

No ordering requirement. Parallelism within each directory.

---

## Validation

- [ ] All workspace unit tests pass
- [ ] On macOS ARM64: `ocx exec cmake -- cmake-gui` launches without dyld errors
- [ ] On macOS ARM64: standalone tool (`ocx exec cmake -- cmake --version`) works
- [ ] On macOS: `OCX_DISABLE_CODESIGN=1` still disables signing

---

## Links

- [Homebrew keg.rb — per-file signing reference](https://raw.githubusercontent.com/Homebrew/brew/master/Library/Homebrew/extend/os/mac/keg.rb)
- [Apple TN2206: macOS Code Signing In Depth](https://developer.apple.com/library/archive/technotes/tn2206/_index.html)
- [Analysis: Team ID mismatch root cause](./analysis_codesign_team_id_mismatch.md)
