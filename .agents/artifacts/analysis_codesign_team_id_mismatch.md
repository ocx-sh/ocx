# Analysis: "different Team IDs" codesign error for cmake-gui Qt frameworks

**Date:** 2026-03-11
**Branch:** fix/codesign-inside-out-signing
**Symptom:** `cmake-gui` fails to launch with dyld error:
```
'QtWidgets.framework/Versions/5/QtWidgets' not valid for use in process:
mapping process and mapped file (non-platform) have different Team IDs
```

---

## Root Cause

### The signing sequence for `CMake.app/Contents/Frameworks/QtWidgets.framework/`

When `sign_directory` processes this framework, the flow is:

1. `sign_directory(QtWidgets.framework/)`:
   - `is_versioned_framework()` → TRUE → **skip step 2** (no individual file signing) ✓
   - `is_bundle()` + `sign_bundle()` → returns early because `is_versioned_framework` → **skip bundle signing** ✗

2. `sign_directory(Versions/)`:
   - No files, no bundle → nothing signed

3. `sign_directory(Versions/5/)`:
   - `is_versioned_framework(Versions/5/)` → FALSE (no `.framework` extension)
   - **Step 2 runs**: signs `QtWidgets` binary with `--preserve-metadata=entitlements,requirements,flags,runtime`
   - `is_framework_version_dir(Versions/5/)` → TRUE → `sign_bundle(Versions/5/)` called

4. `sign_bundle(Versions/5/)`:
   - `codesign --sign - --force Versions/5/`

### Why the binary ends up with a Team ID conflict

The original `QtWidgets.framework/Versions/5/QtWidgets` was signed by Qt/The Qt Company with a real Apple Developer certificate. Their **Designated Requirement (DR)** looks like:
```
identifier "org.qt-project.QtWidgets" and anchor apple generic and
certificate leaf[subject.OU] = "XXXXXXXX"
```
where `XXXXXXXX` is Qt's Team ID.

In step 3's individual signing with `--preserve-metadata=requirements`:
- The binary gets an **ad-hoc signature** (no Team ID)
- But the **DR is preserved** from the original Qt certificate signing, requiring `subject.OU = Qt's Team ID`

When macOS later loads `QtWidgets` into `cmake-gui` (which has a clean ad-hoc signature, no Team ID), it evaluates the DR. The DR says "must have Qt's Team ID" but the loading process has no Team ID → **"different Team IDs"**.

### Why commit `0cf16fc` did not fix this

Commit `0cf16fc` added `is_versioned_framework()` to skip signing files at the **top-level `.framework/` directory** (e.g., `QtWidgets.framework/QtWidgets`). That file gets the "ambiguous bundle format" error from codesign.

**But `Versions/5/QtWidgets` is NOT in the top-level framework dir** — it's in `Versions/5/`. `is_versioned_framework(Versions/5/)` → FALSE, so step 2 still runs and still uses `--preserve-metadata=requirements`.

The fix relied on `sign_bundle(Versions/5/)` to override the individual signing. If that call successfully re-signs the binary without `--preserve-metadata`, the requirements would be cleared. However:

- **If** codesign doesn't properly treat `Versions/5/` as a bundle (since it lacks a `.framework` extension, codesign's bundle detection depends on finding `Resources/Info.plist`), the bundle signing might fail silently, leaving the binary with the preserved requirements.
- Even if it succeeds, the individual signing in step 2 first modifies the binary, invalidating the existing `_CodeSignature/CodeResources` (from Qt's original signing). The bundle re-sign then needs to rebuild this seal.

---

## Homebrew Comparison

Homebrew's approach (from `Library/Homebrew/extend/os/mac/keg.rb`):
- Signs **individual Mach-O files only** — no bundle directory signing
- Uses `--preserve-metadata=entitlements,requirements,flags,runtime` (same as us)
- On **Intel**: only re-signs if `codesign --verify` returns `invalid signature` (i.e., only patched binaries)
- On **ARM**: always re-signs (all Mach-O must be signed on Apple Silicon)

Homebrew never gets the Team ID error because:
1. On Intel it skips already-valid signatures
2. It doesn't do bundle sealing, so there's no sign→seal→sign double-signing problem
3. Its binaries are typically built FROM SCRATCH by Homebrew with its own provisioning — the DRs don't reference a foreign Team ID

**Key difference**: We're re-signing THIRD-PARTY binaries (Qt signed by The Qt Company) whose DRs reference their certificate chain. Homebrew builds its own bottles, so the DRs it preserves were written by Homebrew's own signing.

---

## Apple TN2206 Guidance

From [Apple TN2206](https://developer.apple.com/library/archive/technotes/tn2206/_index.html):

> For multi-versioned frameworks, sign each specific version:
> ```
> # Correct:
> codesign -s identity ../FooBarBaz.framework/Versions/A
> ```

The binary inside should be signed AS PART OF the bundle seal at `Versions/5/`, not individually before the bundle seal.

---

## Fix Plan

### Fix 1 (primary): Skip individual file signing inside framework version directories

In `sign_directory`, step 2 should also skip if `is_framework_version_dir(path)` is true.

**Rationale**: Files inside `Versions/5/` are part of the framework bundle. They should be signed ONLY by the `sign_bundle(Versions/5/)` call (step 3), not individually first. The individual pre-signing:
- Uses `--preserve-metadata=requirements` → embeds conflicting Team ID constraint
- Creates a double-signing race: individual sign modifies the binary, invalidating the existing `_CodeSignature/CodeResources`, then `sign_bundle` must rebuild the seal

**Change in `sign_directory`**:
```rust
// Before:
if !is_versioned_framework(path).await {
    // sign files individually

// After:
if !is_versioned_framework(path).await && !is_framework_version_dir(path) {
    // sign files individually
```

### Fix 2 (safety net): Remove `requirements` from `--preserve-metadata`

Remove `requirements` from `sign_binary`'s `--preserve-metadata` flag:
```
Before: "--preserve-metadata=entitlements,requirements,flags,runtime"
After:  "--preserve-metadata=entitlements,flags,runtime"
```

**Rationale**: For ad-hoc signing of THIRD-PARTY binaries, preserving the original certificate-based DR is counterproductive. The DR references a Team ID that the ad-hoc signature cannot satisfy. `entitlements`, `flags`, and `runtime` are still worth preserving (e.g., sandbox entitlements, hardened runtime). Homebrew's use of `--preserve-metadata=requirements` is safe for its own builds but not for third-party certificate-signed binaries.

This is a safety net: even if Fix 1 is correct and the individual signing of framework binaries is skipped, `sign_binary` is still called for standalone binaries from other publishers. Those binaries might also have Team ID DRs. Removing `requirements` ensures they all get clean ad-hoc signatures.

### Impact assessment

- Fix 1 changes which binaries get individually signed (framework version dir binaries are skipped, signed only via bundle seal)
- Fix 2 changes what metadata is preserved when signing any individual binary
- Both fixes are conservative: they produce CLEANER ad-hoc signatures (no conflicting requirements)
- No loss of meaningful behavior: for ad-hoc signing, the original certificate requirements are meaningless

### Test plan

1. Run `cargo nextest run --workspace` to verify no unit test regressions
2. Build cmake package and test on macOS ARM64:
   - `ocx exec cmake -- cmake-gui` should launch without dyld error
   - `codesign --verify --deep CMake.app` should pass
3. Verify `OCX_DISABLE_CODESIGN=1` still works as escape hatch

---

## Files to change

- `crates/ocx_lib/src/codesign.rs` — two targeted changes

---

## Related commits

- `ac46b2a` — initial inside-out signing (replaced `--deep`)
- `1de0d74` — added `is_framework_version_dir` and `sign_bundle(Versions/5/)` approach
- `0cf16fc` — skipped top-level framework file signing (partial fix)
- This fix — skips framework version dir file signing + removes `requirements` from preserve-metadata
