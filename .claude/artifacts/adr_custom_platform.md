# ADR: Custom Platform Type with OperatingSystem and Architecture Enums

## Metadata

**Status:** Proposed
**Date:** 2026-03-22
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** api
**Supersedes:** N/A
**Superseded By:** N/A

## Context

`oci::Platform` wraps `Option<oci_client::manifest::Platform>` to add OCX's `"any"` sentinel concept. While the `Identifier` type was fully replaced with OCX-owned fields (see `adr_custom_oci_identifier.md`), `Platform` still delegates storage, field access, and serde to the upstream `oci_client::manifest::Platform`.

The upstream type was designed for the OCI Image Index specification and carries design choices that conflict with OCX's evolving needs:

1. **Open enums with `Other(String)` fallback.** `oci_client::config::Os` and `Architecture` accept arbitrary strings. OCX maintains a runtime whitelist (`os_variants()`, `arch_variants()`) that rejects unknowns — but the type system doesn't enforce this. A mistyped `"linus"` compiles fine and only fails at runtime.

2. **No custom matching semantics.** `Platform::matches()` is currently strict equality. OCX needs richer matching: a package built for `linux/amd64` with `os_version: "glibc-2.17"` should match a host with `glibc-2.31`. The upstream struct provides no hooks for this — `variant`, `os_version`, and `os_features` are unstructured `String`/`Vec<String>` fields with no matching semantics.

3. **Leaky abstraction.** `inner` is `pub(crate)` and accessed directly in `manifest.rs`, `client.rs`, and all `From`/`TryFrom` impls. The `Hash` implementation routes through `to_string()` because the upstream type doesn't implement `Hash`.

4. **Future requirements.** OCX's platform concept will extend beyond the OCI `os/architecture` pair to include OS requirements (minimum glibc, kernel version) and feature capabilities (CUDA, AVX2) that inform smarter install-time matching. These have no natural representation in the upstream type.

## Decision Drivers

- **Type safety**: Compile-time guarantee that only supported OS/arch values exist, eliminating the `Other(String)` footgun
- **OCI compatibility**: JSON serialization in OCI manifests MUST remain identical — registries and other OCI tools must be able to read our manifests
- **Extensibility**: `variant`, `os_version`, and `os_features` fields provide room for richer matching semantics without inventing new OCI fields
- **Consistency**: Follow the `Identifier` precedent — OCX-owned types with conversion at the transport boundary
- **Module clarity**: Prefer deep nested modules (one concept per file) over monolithic files

## Industry Context & Research

**Research artifact:** N/A (domain-specific decision, not technology selection)
**Trending approaches:** The OCI spec's `platform` object is stable but limited. Tools like Nix (system strings like `x86_64-linux`), Bazel (constraint values), and Homebrew (OS/arch + feature flags) all use richer platform models than OCI provides.
**Key insight:** OCX already wraps Platform — the question is whether to complete the ownership pattern established by `Identifier`, not whether to start wrapping.

## Considered Options

### Option A: Full ownership with own OperatingSystem and Architecture enums

**Description:** Replace `Option<native::Platform>` internals with OCX-owned `OperatingSystem` enum, `Architecture` enum, and structured fields. Each type gets its own module file. Conversion to/from `native::Platform` happens at the OCI transport boundary only. JSON serialization for OCI manifests remains unchanged (field names `os`, `architecture`, `os.version`, `os.features`, `variant`, `features`).

```
oci/
├── platform.rs                    # Platform struct, PlatformKind enum
├── platform/
│   ├── operating_system.rs        # OperatingSystem enum + Display/FromStr
│   └── architecture.rs            # Architecture enum + Display/FromStr
```

| Pros | Cons |
|------|------|
| Closed enums — invalid OS/arch is a compile error | Must maintain bidirectional mapping to native enums (~20 lines each) |
| `Hash`, `Ord`, `Display`, `FromStr` derived naturally | Two representations exist (like Identifier) |
| `matches()` can use structured fields for richer logic | Slightly more code than the wrapper approach |
| Module-per-concept matches project conventions | |
| Serde can be implemented to preserve OCI JSON format | |
| `variant`, `os_version`, `os_features` available for future matching | |

### Option B: Own OperatingSystem/Architecture enums but keep native::Platform as inner storage

**Description:** Create OCX enums for `OperatingSystem` and `Architecture` used in public APIs and matching logic, but keep `native::Platform` as the internal storage format. The enums serve as validated views over the native data.

| Pros | Cons |
|------|------|
| Less refactoring — inner storage unchanged | Two levels of types (OCX enums + native enums) with mapping in both directions |
| Transport boundary conversion is identity | Still depends on native type for serde, Hash workaround remains |
| | `variant`/`os_version`/`os_features` still unstructured native strings |
| | Doesn't fully resolve the leaky abstraction |

### Option C: Status quo (keep wrapping native::Platform)

**Description:** Keep the current `Option<native::Platform>` wrapper. Add richer matching logic by inspecting native fields directly.

| Pros | Cons |
|------|------|
| Zero refactoring | `Other(String)` remains a type-level footgun |
| | Hash via `to_string()` workaround persists |
| | Matching logic must work around unstructured native fields |
| | Inconsistent with the Identifier pattern |

## Decision Outcome

**Chosen Option:** Option A — Full ownership with own OperatingSystem and Architecture enums

**Rationale:** This completes the pattern established by `Identifier`. The overhead is modest (two small enum files + conversion impls), the type safety gain is real, and it unblocks future matching enhancements without fighting upstream types. Option B creates more complexity (three layers of types) for less benefit. Option C leaves known pain points unresolved.

### Consequences

**Positive:**
- Compile-time enforcement of supported OS/arch values — `os_variants()` and `arch_variants()` become unnecessary
- `Hash`, `Eq`, `Ord` derived directly on enums — no `to_string()` workaround
- `matches()` can inspect `os_version`, `os_features`, `variant` with structured logic
- Consistent ownership pattern across all OCX-wrapped OCI types
- Deep module structure (`platform/operating_system.rs`, `platform/architecture.rs`) matches project conventions

**Negative:**
- Bidirectional mapping between OCX enums and `native::Os`/`native::Arch` must be maintained
- Adding a new OS or architecture requires changes in two places (OCX enum + mapping)

**Risks:**
- **Serde divergence from OCI spec** — Mitigated by: custom `Serialize`/`Deserialize` implementations that produce the exact OCI JSON field names (`"os"`, `"architecture"`, `"os.version"`, `"os.features"`, `"variant"`, `"features"`). Validated by roundtrip tests against known OCI manifest JSON.

## Technical Details

### Module Structure

```
crates/ocx_lib/src/oci/
├── platform.rs                         # Platform struct, serde, Display, FromStr, matches()
├── platform/
│   ├── operating_system.rs             # OperatingSystem enum
│   └── architecture.rs                 # Architecture enum
```

### Type Design

```rust
// oci/platform/operating_system.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OperatingSystem {
    Linux,
    Darwin,
    Windows,
}
// Display: "linux", "darwin", "windows" (lowercase, OCI-compatible)
// FromStr: parses the same strings
// Serde: serializes as the Display string

// oci/platform/architecture.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Architecture {
    Amd64,
    Arm64,
}
// Display: "amd64", "arm64" (OCI-compatible)
// FromStr: parses the same strings
// Serde: serializes as the Display string
```

```rust
// oci/platform.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Platform {
    Any,
    Specific {
        os: OperatingSystem,
        arch: Architecture,
        variant: Option<String>,
        os_version: Option<String>,
        os_features: Option<Vec<String>>,
    },
}
```

### Serde — OCI JSON Compatibility

The OCI Image Index `platform` object has this JSON shape:

```json
{
  "architecture": "amd64",
  "os": "linux",
  "os.version": "10.0.14393.1066",
  "os.features": ["win32k"],
  "variant": "v7",
  "features": []
}
```

`Platform::Specific` serializes to this exact shape using a custom `Serialize`/`Deserialize` implementation (or a `#[serde(into/from)]` helper struct). `Platform::Any` serializes as `{"os": "any", "architecture": "any"}` for OCI compatibility, matching the current behavior.

The human-readable string format (`"linux/amd64"`, `"any"`) used in CLI `--platform` flags and `Display`/`FromStr` remains unchanged.

### Transport Boundary Conversions

```rust
// Platform → native::Platform (for OCI push/pull)
impl From<&Platform> for native::Platform { ... }

// native::Platform → Platform (from OCI manifest)
impl TryFrom<native::Platform> for Platform { ... }

// OperatingSystem ↔ native::Os
impl From<OperatingSystem> for native::Os { ... }
impl TryFrom<native::Os> for OperatingSystem { ... }

// Architecture ↔ native::Arch
impl From<Architecture> for native::Arch { ... }
impl TryFrom<native::Arch> for Architecture { ... }
```

### Matching Semantics (Future-Ready)

```rust
impl Platform {
    /// Returns true if `self` is compatible with `candidate`.
    ///
    /// Rules:
    /// - Any matches Any
    /// - Any matches Specific (platform-agnostic packages run anywhere)
    /// - Specific matches Specific if os + arch match, and:
    ///   - variant matches (if specified on self)
    ///   - os_version of candidate >= os_version of self (if specified)
    ///   - os_features of self is a subset of candidate's os_features
    pub fn matches(&self, candidate: &Platform) -> bool { ... }
}
```

Initial implementation: equality only (same as today). The struct enables richer matching without type changes.

### Public API Surface Changes

| Before | After |
|--------|-------|
| `Platform::os_variants() -> Vec<native::Os>` | Removed — enum variants are the source of truth |
| `Platform::arch_variants() -> Vec<native::Arch>` | Removed — enum variants are the source of truth |
| `Platform::current() -> Option<Self>` | Returns `Platform::Specific { .. }` instead of wrapping native |
| `Platform::any() -> Self` | Returns `Platform::Any` |
| `Platform::is_any(&self) -> bool` | `matches!(self, Platform::Any)` |
| `platform.inner` (`pub(crate)`) | Removed — no inner field |
| `From<Platform> for native::Platform` | `From<&Platform> for native::Platform` (borrow) |

### Re-exports in `oci.rs`

```rust
// oci.rs — public API
mod platform;
pub use platform::Platform;
pub use platform::operating_system::OperatingSystem;
pub use platform::architecture::Architecture;

// native module — no longer re-exports Os/Arch (or kept for oci-client interop only)
```

## Implementation Plan

1. [ ] Create `oci/platform/operating_system.rs` — `OperatingSystem` enum with `Display`, `FromStr`, `Serialize`, `Deserialize`, and conversion impls to/from `native::Os`
2. [ ] Create `oci/platform/architecture.rs` — `Architecture` enum with same traits and conversions to/from `native::Arch`
3. [ ] Refactor `oci/platform.rs` — Replace `Option<native::Platform>` with `Platform` enum (`Any` / `Specific`). Implement custom serde to preserve OCI JSON format. Update `Display`, `FromStr`, `matches()`, `segments()`, `current()`
4. [ ] Update transport boundary — `From<&Platform> for native::Platform`, `TryFrom<native::Platform> for Platform` using new enum conversions
5. [ ] Update callsites — `oci/client.rs`, `oci/manifest.rs`, `oci/index.rs`, `package/info.rs`, `package/cascade.rs` — replace `platform.inner` access with public methods
6. [ ] Update `oci.rs` re-exports — add `OperatingSystem`, `Architecture`; evaluate whether `native::Os`/`native::Arch` re-exports are still needed
7. [ ] Update `ocx_cli` — `conventions.rs` (`supported_platforms`), command modules using `--platform`
8. [ ] Update `ocx_mirror` — `AssetPatterns`, `VersionPlatformMap`, `StripComponentsConfig` (migrate from raw strings to `Platform`)
9. [ ] Remove dead code — `os_variants()`, `arch_variants()`, `oci_platform_is_any()`, error variants `PlatformInvalidOs`/`PlatformInvalidArch` (merged into `PlatformUnsupported` or replaced by `FromStr` errors)
10. [ ] Add roundtrip tests — OCI JSON serde roundtrip, `Display`/`FromStr` roundtrip, native conversion roundtrip
11. [ ] Run `task verify` — all quality gates must pass

## Validation

- [ ] OCI JSON roundtrip: serialize `Platform` → JSON → deserialize → assert equal (for all supported combinations)
- [ ] OCI JSON field names: assert serialized JSON contains `"os"`, `"architecture"`, `"os.version"`, `"os.features"`, `"variant"` (not Rust field names)
- [ ] Native roundtrip: `Platform` → `native::Platform` → `Platform` → assert equal
- [ ] `Display`/`FromStr` roundtrip: `"linux/amd64"` → `Platform` → `String` → `Platform` → assert equal
- [ ] `"any"` handling: `Platform::Any` serializes to `{"os":"any","architecture":"any"}` in OCI context
- [ ] All existing acceptance tests pass unchanged (they use `--platform linux/amd64` string parsing)

## Links

- [ADR: Custom OCI Identifier](./adr_custom_oci_identifier.md) — precedent for this pattern
- [OCI Image Index Spec — Platform Object](https://github.com/opencontainers/image-spec/blob/main/image-index.md#image-index-property-descriptions)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-22 | mherwig + AI | Initial draft |
