# ADR: libc Family Differentiation via `os.features` with `libc.*` Namespace

## Metadata

**Status:** Accepted
**Date:** 2026-05-28
**Deciders:** mherwig
**Beads Issue:** TBD
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024 / Tokio; no new external dep — uses existing OCI fields)
**Domain Tags:** oci, platform, resolution
**Supersedes:** N/A
**Superseded By:** N/A

---

## Context

OCX's `Platform` type at `crates/ocx_lib/src/oci/platform.rs` already carries the full OCI Image Index platform shape — `os`, `arch`, `variant`, `os_version`, `os_features`, `features` — but only `os` + `arch` (and partially `variant` via segments) participate in resolution. The matcher (`Platform::matches`, line 171) is strict equality; the only production call site is `Index::select` at `crates/ocx_lib/src/oci/index.rs:254`, which walks `supported_set()` against candidate platforms from `Manifest::ImageIndex` entries.

This is sufficient for a single libc world. It fails the moment a Linux package needs to ship distinct glibc and musl binaries: today both have to be tagged as `linux/amd64` in the index, the runtime picks one nondeterministically (by manifest order), and one of the two host populations gets the wrong binary — a clean failure at `ld.so` (`version 'GLIBC_X.Y' not found`), but a failure nonetheless.

OCI registries are the distribution backbone OCX is built on. We need the registry to be able to answer "which manifest does this host want?" in a single pass without client-side annotation walking. That means encoding libc identity inside the OCI `platform` object itself.

### Current State (from architecture discovery)

| Surface | File:line | Note |
|---|---|---|
| Matcher | `platform.rs:171` | Strict equality; pass-through for `os_features` + `features` |
| Single production caller | `index.rs:254` | Inside `Index::select` priority loop |
| `supported_set()` | `platform.rs:207–214` | Currently returns `[current(), Any]` — ordering already used as priority |
| `supported_set()` consumers | `cli/conventions.rs:74`, `package_manager/tasks/update_check.rs:329` | All resolution paths via CLI + self-update |
| Second equality path | `oci/manifest.rs:13` | `has_platform` uses `native::Platform` struct equality; out-of-band; must stay consistent |
| Host capability detection | (none) | No CPUID/proc/ldd reads exist anywhere in codebase — greenfield |
| Mirror publish injection | `ocx_mirror/src/pipeline/mirror_task.rs:27` (field), `pipeline/orchestrator.rs:363–367` (assembly) | Single point where platform metadata is written for publish |

### What the OCI spec actually allows (v1.1.1, Nov 2025)

Researched and persisted at [`research_libc_platform_differentiation.md`](./research_libc_platform_differentiation.md). Headline facts:

- `os.features` (OPTIONAL): for `windows`, SHOULD use `win32k`. **For non-windows, "values are implementation-defined and SHOULD be submitted to this specification for standardization."** We are explicitly invited.
- `features` (OPTIONAL): **"RESERVED for future versions of the specification."** Untouchable.
- `variant` (OPTIONAL): spec-blessed for CPU microarch (`amd64 v1/v2/v3/...`, `arm64 v8, v8.1, ...`). Off-limits for ABI/libc.
- No ecosystem project uses OCI platform fields for libc today. Universal current practice is separate tags (`tool:1.0` vs `tool:1.0-musl`) or separate URL buckets. Genuine gap in the spec ecosystem.

## Decision Drivers

- **D1. Single-pass OCI resolution.** Index must be able to select the exact manifest for the host platform without out-of-band lookups. Annotations on descriptors fail this test.
- **D2. Spec compliance.** Stay within OCI v1.1.1 normative boundaries. Anything we ship should be submittable upstream as a standardization proposal.
- **D3. Predictable matching.** Host detection must be a one-rule heuristic, falsifiable, and not require equivalence inference between libc implementations.
- **D4. Backward compatibility.** Existing OCX-published artifacts (with empty `os_features`) must keep resolving correctly when a libc-aware host is introduced.
- **D5. Forward compatibility.** Naming and semantics must accommodate future axes (libc version, Windows runtime distinctions, CPU microarch via `variant`) without renaming.
- **D6. Cross-platform symmetry.** libc as a concept is OS-orthogonal (Linux has glibc/musl; Windows has UCRT/MSVCRT/Cygwin/MSYS2; macOS has libSystem). Naming must not assume Linux.
- **D7. Author burden minimization.** Package publishers should set one or two simple values per manifest entry; OCX selects automatically. No SAT solver, no closure modeling.
- **D8. Bounded scope.** OCX is a distributor, not a toolchain builder. Shared-library dependency modeling (libatomic, libstdc++) and Nix-style hermetic packaging are out of scope for this ADR.

## Industry Context & Research

**Research artifact:** [`research_libc_platform_differentiation.md`](./research_libc_platform_differentiation.md)

**Trending approaches:**

- Container ecosystem (Alpine, Wolfi, Chainguard, Docker Hardened Images) — universally use *separate tags or separate repos* for glibc vs musl, never OCI platform fields.
- Rust binary tooling (cargo-binstall, uv, ruff, cargo-dist) — encode libc in URL artifact names (Rust target triple), detect host libc at install time via `ld.so` probe. No OCI involvement.
- Python wheels (PEP 600 / PEP 656, `manylinux_X_Y_arch` / `musllinux_X_Y_arch`) — the closest existing prior art: per-(libc-family, libc-min-version, arch) tags with subset/superset compatibility resolution. Wheels emitted in descending version order; installer picks first-match.
- OCI spec (open issue #1216, ORAS-driven) — explores annotations for *platform identification on single-manifest artifacts*. Distinct problem (no index parent); no libc scope.

**Key insight:** The OCI `os.features` field has been sitting idle for non-Windows OSes since v1.0, explicitly inviting implementation-defined values that "SHOULD be submitted to the spec for standardization." Container tooling never picked it up because their selection happens distro-side (Alpine *is* musl; the choice is the image, not a field). For a *single-manifest-distributing-binaries* tool like OCX, however, in-platform tagging is the only mechanism that keeps index resolution single-pass. We are the natural first user of `os.features` for libc; we should ship the pattern and submit it upstream.

## Considered Options

### Option 1: `os.features` + `libc.*` namespace, subset matching (CHOSEN)

**Description:** Encode libc family as values in `platform.os.features` using a concept-namespaced scheme: `libc.musl`, `libc.glibc`, with `libc.ucrt`/`libc.cygwin`/`libc.msys2`/`libc.apple` reserved for future Windows/macOS use. Matching is subset semantics: `candidate.os_features ⊆ host.os_features`. Host detection at runtime via cargo-binstall's `ld.so` probe pattern.

| Pros | Cons |
|------|------|
| Single-pass index resolution preserved (D1) | Container tooling will ignore the field (no interop benefit beyond OCX) |
| Spec-compliant; explicitly invited by v1.1.1 normative text (D2) | First implementor — small risk of namespace collision if upstream standardizes differently |
| OS-orthogonal naming extends to Cygwin/MSYS2/UCRT/libSystem (D5, D6) | Subset matching is a new pattern in the codebase (D7-cost) |
| Predictable detection rule: "what ld.so identifies as" (D3) | Host detection adds Linux-specific runtime probing (~1 module) |
| Existing `os_features` field — no new field invented (D2) | Empty-set fallback semantics need clear documentation |
| Backward compatible via empty-set ⊆ any-set (D4) | |
| Composes cleanly with future `os.version` for libc version ranges (D5) | |

### Option 2: Annotations on the manifest descriptor (`io.ocx.libc`)

**Description:** Publish each platform entry with identical `platform.os`/`platform.architecture`/`platform.variant`, distinguished only by an OCI annotation on the descriptor (e.g. `"annotations": {"io.ocx.libc": "musl"}`).

| Pros | Cons |
|------|------|
| Annotation namespace exists, well-precedented | **Breaks single-pass index resolution (D1 fail)** — entries with identical platforms are formally ambiguous; runtime picks first |
| No platform-field invention | Forces OCX to fetch all matching descriptors, read annotations, re-filter client-side |
| Interop-safe (other tooling ignores annotations cleanly) | Breaks interop with `docker manifest`, `oras manifest fetch --platform` |
| | Sets a precedent of using annotations for selection — slippery slope |

Verdict: **Rejected**. Fails the architectural primary driver.

### Option 3: Encode libc in `variant`

**Description:** Use `variant: "musl"` and `variant: "glibc"` on amd64 entries.

| Pros | Cons |
|------|------|
| In-platform — keeps single-pass resolution (D1 satisfied) | **Collides with spec-defined CPU microarch values** (`v1/v2/v3/v4` for amd64) |
| Single field, minimal change | Mixes ABI dimension with ISA dimension; breaks composition with future variant-based CPU tier selection |
| | Off-spec — variant values are normatively listed in the Platform Variants table |

Verdict: **Rejected**. Closes the door on `variant` for CPU microarch (which is the spec's blessed use).

### Option 4: Encode libc in the `features` field

**Description:** Put `["musl"]` or `["glibc"]` in the `platform.features` array.

| Pros | Cons |
|------|------|
| Semantically the closest fit ("CPU/ABI feature requirements") | **`features` is RESERVED in OCI v1.1.1** ("This property is RESERVED for future versions of the specification.") |
| Looks tidy | Any future spec use will collide and break us |

Verdict: **Rejected**. Spec forbids.

### Option 5: Tag suffixes only (`tool:1.0` vs `tool:1.0-musl`)

**Description:** Publish each libc variant under a distinct tag. No platform-field changes.

| Pros | Cons |
|------|------|
| Industry-universal pattern; zero risk | **Loses auto-resolution** — user must know suffix taxonomy and pass it manually |
| Human-readable | Bifurcates the tag namespace (`tool:1.0-musl`, `tool:1.0-glibc`, `tool:1.0-musl-static` ...) |
| | Works against OCX's "backend-first, single command" principle (`product-context.md` Principle 1, 5, 6) |
| | Doesn't preclude future `os.features` adoption — but defers it |

Verdict: **Rejected as primary mechanism**, retained as optional human-readable alias. Publishers may still emit `tool:1.0-musl` as a courtesy tag pointing at the same digest; OCX's auto-resolution will pick the right entry from `tool:1.0` regardless.

### Option 6: Nix-style bundled libc with patchelf

**Description:** Each OCX package ships its own libc closure; OCX patches the binary's `PT_INTERP` and `RPATH` at install time to point at the bundled loader.

| Pros | Cons |
|------|------|
| Truly hermetic; host libc irrelevant | **No portable equivalent on macOS or Windows** — `patchelf` is Linux-only. `install_name_tool` rewrites dyld paths but can't substitute bundled libc the same way. Windows has no kernel-side loader rewrite. |
| Works on NixOS where host detection fails | Closure bloat: ~30-50 MB libc-and-friends *per package*; multi-GB installs |
| Solves "musl host, glibc package" by bundling | OCX would need to maintain blessed libc builds per triple, or trust publisher-supplied libc (large attack surface) |
| | Mission creep: OCX becomes a toolchain rebuilder, not just a distributor |
| | Doesn't free us from kernel ABI: new glibc on old kernel still breaks |

Verdict: **Rejected for v1**. Conceptual slot reserved (`runtime.bundled_libc: true` as a future annotation) but not implemented. Promote static-musl publishing as the "runs anywhere" idiom instead — same end result, zero infrastructure.

### Option 7: OS-prefixed namespace (`linux.musl`, `linux.glibc`)

**Description:** Same as Option 1 but with OS-scoped value prefixes.

| Pros | Cons |
|------|------|
| Matches `win32k`'s implicit-OS pattern | **Breaks for Cygwin / MSYS2 / UCRT — those are `os: windows` libcs** (D6 fail) |
| | Forces renames the moment we touch non-Linux libc differentiation |

Verdict: **Rejected** in favor of OS-orthogonal `libc.*`.

## Decision Outcome

**Chosen Option:** Option 1 — `os.features` + `libc.*` namespace, subset matching.

**Rationale:** Only option that satisfies D1 (single-pass resolution) while staying within the OCI spec (D2). The `libc.*` namespace decouples libc identity from OS identity, future-proofing for Cygwin/MSYS2/UCRT/libSystem (D5, D6). Subset matching maps cleanly onto the spec's "mandatory OS feature host must provide" wording. Backward compatibility falls out of subset semantics (empty entry matches everything). Host detection uses an established pattern (cargo-binstall), so the runtime probe has a proven implementation reference.

### Concrete decisions locked

| Item | Value |
|---|---|
| Field | `platform.os.features` (existing OCI field) |
| Namespace | `libc.*` — OS-orthogonal |
| Initial values | `libc.musl`, `libc.glibc` |
| Future values (reserved, not implemented) | `libc.ucrt`, `libc.msvcrt`, `libc.cygwin`, `libc.msys2`, `libc.apple` |
| Forbidden | `platform.features` field (RESERVED per OCI v1.1.1) — stop serializing |
| Match semantics | Subset: `candidate.os_features ⊆ host.os_features` |
| `supported_set()` | Returns `[current(), Any]` — current carries detected `os_features`, Any unchanged |
| Empty-features entry | Matches every host (encourages static-link best practice) |
| gcompat / equivalents | Host reports the loader it actually has (e.g. Alpine+gcompat → `libc.musl` only); no equivalence inference |
| Tiebreaker between multiple matching entries | Lexicographic specificity score `(is_specific, matched_os_feature_count)` — highest wins; a tie surfaces as `Ambiguous` (exit 65). Supersedes the original "manifest array index order" tiebreak, which never matched the shipped `Index::select`. Locked in [`adr_platform_model_unification.md`](./adr_platform_model_unification.md) D1. |
| Multi-libc host | Detection is a set-union with no preserved discovery order; a dual-libc host presents ONE `current()` candidate carrying the combined `os.features` (e.g. `["libc.glibc","libc.musl"]`). No loader-order/first-wins preference — it matches a combined-feature entry exactly, or, against two equally-specific single-libc entries, `Index::select` returns `Ambiguous` (exit 65), resolved via explicit `--platform` |
| Host detection | New module: cargo-binstall pattern (parallel ld.so probes, parse `--version` output). Linux-only; macOS/Windows have static libc family. |
| `Platform::matches()` replacement | `Platform::can_run(other)` — subset semantics over `os_features` + equality on `os` + `arch` (`self` can run `other`) |
| `os_version` matching | **Superseded** (`adr_platform_model_unification.md`, 2026-07-17): the `os_version` axis is deleted from `Platform::Specific` entirely — no matching rule exists. |
| Versions (e.g. glibc ≥ 2.17) | **Deferred**. Any future version-range axis needs a new ADR (`adr_platform_model_unification.md` removed `os.version` from the model). Today's `libc.musl`/`libc.glibc` stay valid. |
| CPU microarch variants | **Deferred**. Spec-blessed via `variant` field; orthogonal to libc; revisit in separate ADR. |
| Shared-library closure beyond libc | **Out of scope.** Package authors static-link. |
| Spec upstream submission | Ship first; submit `libc.*` namespace proposal to image-spec issue later, citing OCX as reference impl. |

### Quantified Impact

Not directly applicable (no perf/cost metric). Functional impact:

| Surface | Before | After |
|---|---|---|
| Distinct libc binaries per package | Force separate tags (`-musl` suffix) or single-libc-only | Co-existing in same multi-platform index, auto-selected |
| Wrong-libc pulls | Possible (silent, fails at `ld.so` runtime) | Eliminated when **both** host detection succeeds AND publisher tags entries correctly. Mis-tagged publishes still produce wrong-libc pulls (publisher bug, not OCX failure). Best-effort publisher-side ELF lint covered in Phase 8 stretch of the implementation plan. |
| Old-publisher artifacts on new host | (n/a) | Still pulled correctly (empty-set ⊆ host-set) |
| New-publisher artifacts on old host | (n/a) | Pull fails cleanly; user falls back to `--platform` override |

### `os.features` interpretation is lenient

`os.features` interpretation is lenient: unrecognized features carry no semantic meaning and never error; the `libc.*` namespace is modeled as an enum with an `Unknown` variant; wire format (string array) is unchanged. Host detection still only ever produces `Glibc`/`Musl` — `Unknown` arises solely when *interpreting* an inbound `libc.*` tag OCX does not recognise. A typed `Feature::{Libc, Other}` model decodes a single tag for reporting paths (`ocx about` / `ocx version` libc extraction) without ever failing; subset *matching* in the resolver stays string-based, so an `Other`/`Unknown` feature simply does not match rather than erroring.

### Consequences

**Positive:**

- Index resolution stays single-pass; `Index::select` continues to be the one authoritative selection point.
- OCX becomes the first OCI consumer to populate `os.features` for non-Windows libc — concrete contribution we can submit upstream.
- Static-musl binaries become the "universal fallback" idiom: publish with `os.features: []` and the binary matches every Linux host regardless of detected libc.
- Future axes (libc version via `os.version`; CPU variants via `variant`) plug in without renaming what's locked here.
- `Platform::Specific.features` field stops being serialized — closes a latent spec violation discovered during research.

**Negative:**

- New runtime dependency: host libc detection adds ~1 module and a small set of subprocess calls at startup (or first resolution). Cost: ~5-20 ms on Linux first call; cached after.
- Subset matching is the first non-equality matcher in the codebase. Sets a precedent that may be misapplied to other fields (tags, identifiers) by future maintainers. Mitigation: scope narrowly to `os_features`; doc-comment on `can_run` + an explicit rule in `subsystem-oci.md` stating subset semantics apply to `os_features` only and any extension to other fields requires a new ADR.
- One open spec-compliance risk: if the OCI working group later standardizes a different value scheme for `os.features` libc tags, we migrate. Mitigation: submit a proposal early; until then, `libc.*` is conservative naming unlikely to collide.

**Rejected during review:**

- *Vendor-prefixed namespace `io.ocx.libc.*`* — considered to reduce blast radius if OCI standardizes a different name. Rejected: `os.features` precedent is bare values (`win32k`); reverse-DNS conventions live in OCI *annotations*, not in this field; migration cost via client-side alias is bounded. Vendor prefix would be uglier without compelling protection. Document monitoring of OCI issue #1237 as the trigger condition for rename.

**Risks:**

| Risk | Mitigation |
|---|---|
| Dep-resolution greedy-fail corner (package A's glibc entry picks, dep B has no glibc entry → fails; musl path would have worked) | Document as known limitation. Lint at `ocx package push` time. Full SAT backtracking out of scope. |
| NixOS host detection (no `/lib/ld-linux-*.so.2` paths) | Detection returns `None`; subset rule then matches empty-features entries only. User can override via `--platform`. Future: parse `/proc/self/exe` PT_INTERP for fallback. |
| Mirror tool publisher mis-tagging (publishes glibc-built binary as `libc.musl`) | Author responsibility. Optional future: `ocx package push` could ELF-parse PT_INTERP and warn on mismatch. |
| Old OCX clients with new libc-tagged registries | Old client treats `os_features` as opaque pass-through (current behavior); strict equality at `Platform::matches` won't match `libc.*` entries unless host also reports them. Old client gets fallback (`Any`) or no-match error. Acceptable degradation. |
| Existing test `serde_features_accepted_and_roundtripped` (and `native_roundtrip_with_features`) assert wrong behavior (populating RESERVED field) | Rewrite tests during impl; the round-trip should test that round-trip *preserves None* on Specific.features, or remove the field's serialization path. |
| Cascade equality on `os_features` array is order-sensitive (`oci/client.rs:246` uses `native::Platform` struct equality; `Vec` equality is positional) — re-push with reordered array would fail to evict prior entry, bloating the image index | Normalize `os_features` (sort + dedup) inside `From<&Platform> for native::Platform`. Add `debug_assert!(is_sorted)` on the parse path. Add cascade re-push test asserting eviction. Phase 4 implementation step covers this. |
| Publisher mis-tagging — declared `libc.musl` but binary links glibc loader | Out of v1 *matching* scope (matcher trusts the tag). Implementation plan Phase 8 (stretch) adds best-effort ELF/PT_INTERP lint at `ocx package push` time. Until then, package author responsibility. |
| OCI spec issue [#1237](https://github.com/opencontainers/image-spec/issues/1237) may narrow `os.features` to `[A-Za-z0-9]+` (no dots) | Monitor. Containerd's `platforms/#16` (merged March 2026) did not enforce a character set, so risk is low. Migration path if narrowed: rename `libc.glibc` → `libc_glibc` (one-line in `os_features()`); OCX clients alias both during transition window. |
| `patch sync` without `--platform` cannot pin libc-tagged companions — `Platform::all_supported()` enumerates only empty-`os_features` platforms plus `Any`, so a `{libc.glibc}` candidate fails the subset check (`{libc.glibc} ⊄ {}`) and `Any` never satisfies a `Specific` candidate. A libc-tagged companion is silently skipped. | Known limitation. The code fix (populating `all_supported()` with libc-tagged variants) is deliberately deferred to keep the `can_run` invariant narrow — widening the default supported set is a scope decision for a follow-up ADR. Workaround: pass `--platform linux/amd64+libc.glibc` (or `+libc.musl`) explicitly to `patch sync`. |

## Technical Details

### Architecture

```
                ┌────────────────────────────┐
                │  HostCapabilities::detect  │  ← new module
                │  (cargo-binstall pattern)  │
                └────────────┬───────────────┘
                             │ Option<LibcFlavor>
                             ▼
┌─────────────────────────────────────────────┐
│  Platform::current() — extended              │
│    populates os_features with libc.<flavor>  │
└────────────┬─────────────────────────────────┘
             │
             ▼
┌─────────────────────────────────────────────┐
│  Platform::supported_set() — unchanged shape │
│    [current_with_libc_tags, Any]             │
└────────────┬─────────────────────────────────┘
             │ used as priority list
             ▼
┌─────────────────────────────────────────────┐
│  Index::select (oci/index.rs:254)            │
│    for tier in supported_set:                │
│      for entry in manifests:                 │
│        if tier.can_run(entry):  ←──── new subset matcher
│           return entry                       │
└──────────────────────────────────────────────┘
```

### API Contract — new + changed types

```rust
// crates/ocx_lib/src/oci/platform.rs

impl Platform {
    /// Replaces `matches`. Subset semantics on `os_features`.
    /// Returns true iff `self` can run `other` (every os_feature `other`
    /// requires is present on `self`, plus os/arch/variant rules).
    /// At resolution time `self` = host tier (from `supported_set()`),
    /// `other` = an index entry's platform.
    pub fn can_run(&self, other: &Platform) -> bool { /* … */ }

    // existing `matches()` deleted (pre-1.0).
}
```

```rust
// new module: crates/ocx_lib/src/oci/host_capabilities.rs

// v1: unit variants. Future tuple form (e.g. `Glibc(GlibcVersion)`) is
// deferred — see Notes in the implementation plan. Migrating from unit to
// tuple is a breaking change; acceptable pre-1.0.
pub enum LibcFlavor {
    Glibc,
    Musl,
}

pub struct HostCapabilities {
    pub libc: Option<LibcFlavor>,
}

impl HostCapabilities {
    /// Cargo-binstall-style detection. Linux-only; returns
    /// HostCapabilities { libc: None } on non-Linux.
    pub async fn detect() -> Self { /* … */ }

    /// Map detected libc to `os.features` tag values.
    ///
    ///   - `[]`               — libc undetected; Platform::current() gets empty os_features
    ///   - `["libc.glibc"]`   — Glibc
    ///   - `["libc.musl"]`    — Musl
    ///
    /// `Platform::Specific.os_features` is a plain `Vec<String>`; an empty
    /// `Vec` means "no features / undetected". The native/OCI boundary
    /// converts empty ↔ `None` so the wire field is omitted when empty.
    pub fn os_features(&self) -> Vec<String> { /* … */ }
}
```

```rust
// crates/ocx_lib/src/oci/platform.rs

impl Platform {
    pub fn current() -> Option<Self> {
        let os = OperatingSystem::current()?;
        let arch = Architecture::current()?;
        // Read os_features from a process-wide once-cell populated by
        // HostCapabilities::detect() during context init.
        let os_features = host_capabilities::cached_os_features();
        Some(Self::Specific { os, arch, variant: None, os_version: None,
                              os_features, features: None })
    }
}
```

### Data Model — wire format

OCI Image Index manifest entry, glibc Linux amd64 build:

```json
{
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "digest": "sha256:…",
  "size": 1234,
  "platform": {
    "architecture": "amd64",
    "os": "linux",
    "os.features": ["libc.glibc"]
  }
}
```

Static-musl Linux amd64 build (matches every Linux host):

```json
{
  "platform": {
    "architecture": "amd64",
    "os": "linux"
  }
}
```

Wholly platform-agnostic (Java JAR, Python script):

```json
{
  "platform": {
    "architecture": "any",
    "os": "any"
  }
}
```

### Subset matcher pseudocode

```
can_run(self: Platform, other: Platform) -> bool:   // "self can run other"
  // An Any other runs everywhere — self can always run it. This covers both
  // the publisher-declared agnostic case (JARs, scripts, data bundles) and
  // the bare-manifest fallback that oci/index.rs and
  // Platform::from_image_manifest produce when no platform metadata is
  // present in the source. Both flow through this branch identically.
  if other is Any: return true

  // An Any self means current() detection failed entirely (e.g. unsupported
  // OS/arch). Only Any others match — that case was already handled.
  // Specific others are unreachable from an Any self.
  if self is Any: return false

  // Specific ↔ Specific:
  //   equality on os + arch
  //   when set, strict-equality on variant (CPU microarch deferred)
  //   subset on os_features ("mandatory OS feature host must provide" per spec)
  if self.os != other.os: return false
  if self.arch != other.arch: return false
  if other.variant.is_some() and other.variant != self.variant:
    return false                           // strict for now; revisit when variants implemented
  // os_features is a plain Vec; empty == "no requirement".
  return other.os_features.subset_of(self.os_features)
```

Note on `Platform::Any` semantics: the value is a sentinel for truly platform-agnostic content (scripts, JARs, data bundles), but it is also where `oci/index.rs:222–225` lands index entries with no `platform` field and where `Platform::from_image_manifest` lands single-image manifests. Both meanings collapse to the same matcher rule — *Any candidate runs everywhere* — so a Specific host correctly pulls an Any-tagged entry. This unifies the publisher-declared and inferred-from-absence cases without ambiguity. `Any` is still "sacred" in the sense that there is exactly one such sentinel value, not in the sense of being unreachable from Specific hosts. The pseudocode's `if host is Any: return false` branch covers the rare degenerate case where host detection failed entirely.

### `oci/manifest.rs:13 has_platform` — consistency check

`has_platform` does struct-equality on `native::Platform`. For cascade-push correctness (used to decide whether an existing image-index entry needs replacing), strict equality is the correct semantics — we want "is *this exact* platform already in the index?" not "would a host satisfying X match this entry?". No change required, but add a comment noting the two matchers have intentionally different semantics, with cross-reference.

## Implementation Plan

Sketched here; full plan artifact in `plan_platform_libc_os_features.md` when `/swarm-plan` is invoked.

1. [ ] **Phase 1: Host detection module** — `crates/ocx_lib/src/oci/host_capabilities.rs` (or `platform/host.rs`). Implements `HostCapabilities::detect()` via cargo-binstall ld.so probe pattern. Linux-only logic; macOS/Windows return `None`. Add `ocx host info` debug command for visibility. Tests with mock filesystem layout.
2. [ ] **Phase 2: Subset matcher** — Add `Platform::can_run(other)`. Delete `Platform::matches` (pre-1.0, OK to break). Update single caller at `oci/index.rs:254`. Add subset-semantics tests covering empty-set, subset, equal-set, superset (no match), wrong-libc (no match).
3. [ ] **Phase 3: Wire detection into `Platform::current()`** — Read once at context init, cache, populate `os_features` field. Update `update_check.rs:329` if needed.
4. [ ] **Phase 4: Stop serializing `Platform::Specific.features`** — Field becomes internal-only or removed. Rewrite tests at `platform.rs:689–701` (`serde_features_accepted_and_roundtripped`) and `925–939` (`native_roundtrip_with_features`) to assert the field is NOT round-tripped through serde.
5. [ ] **Phase 5: Mirror tool publisher path** — Allow `MirrorTask` / package metadata to declare `libc.*` value. Plumb through `pipeline/orchestrator.rs:363–367` so it lands in published manifest. Docs.
6. [ ] **Phase 6: Acceptance tests** — Add scenario: registry has both `libc.glibc` and `libc.musl` entries under same tag; glibc-detecting host pulls glibc; musl-detecting host pulls musl; undetected host falls back to legacy/empty entry or `--platform` override.
7. [ ] **Phase 7: Docs** — `website/src/docs/reference/platforms.md` (new) explaining the libc convention. Update `arch-principles.md` "Key Concepts" entry for Platform.
8. [ ] **Phase 8 (deferred, separate ADR)** — Submit `libc.*` namespace proposal to opencontainers/image-spec.

## Validation

- [ ] Perf: `HostCapabilities::detect()` benchmarks: <30 ms cold call on Linux, cached thereafter.
- [ ] Security: subprocess spawns of `ld.so` constrained to known canonical paths (`/lib/...`, `/lib64/...`, `/usr/lib...`); refuse user-provided paths; same threat model as cargo-binstall.
- [ ] Spec compliance: `os.features` values are array of strings, no novel JSON shape introduced. Field is OPTIONAL per spec; absent in legacy entries.
- [ ] Backward compat: existing OCX-published artifacts with empty `os_features` resolve correctly on libc-aware hosts (subset rule).
- [ ] Forward compat: subset matcher is composable with future `variant` matching (CPU microarch) — variant equality check stays in the matcher as a separate clause.
- [ ] Test coverage: unit tests on `can_run` cover empty-set, subset, equal-set, superset-mismatch, wrong-libc, Any-handling. Acceptance test covers end-to-end install on a registry with mixed-libc entries.

## Amendment — Detection mechanism v2 (discovery-then-identify)

**Date:** 2026-05-31. Amends the **Host detection** row and the NixOS risk row;
does not change the wire format, the `libc.*` namespace, the subset matcher, or
any published-artifact contract. Research:
[`research_libc_detection_robustness.md`](./research_libc_detection_robustness.md).

### Problem

The v1 detection (`HostCapabilities::detect`) enumerated a **hardcoded FHS path
allowlist** of canonical loader paths. The disk-enumeration *model* is sound,
but the allowlist false-negatives wherever the loader is not at a canonical FHS
path: NixOS / Guix (`/nix/store`, `/gnu/store`), Gentoo Prefix,
Homebrew-on-Linux, conda, custom sysroots, the Bazel hermetic sandbox, and any
future distro that relocates the loader. The set ends up empty → empty
`os_features` → `Any`-only matching, losing libc-aware auto-selection exactly in
the container/virtualized environments OCX targets. Failure is graceful
(`--platform` override exists), so this is an enhancement, not a defect.

### Decision

Replace the single allowlist enumeration with **discovery-then-identify**, the
SOTA approach (Node `detect-libc`, PEP 656 musllinux):

1. **Discovery** — produce a deduplicated candidate-loader-path set from three
   sources, in priority order:
   1. **`PT_INTERP` (primary).** Read the `PT_INTERP` of an ordered allowlist of
      guaranteed-present, dynamically linked system binaries (`/usr/bin/env`,
      `/bin/sh`, `/bin/ls`). The embedded string is the host's exact native
      loader path — found wherever it lives (NixOS, Gentoo Prefix, custom
      sysroots). A static binary has no `PT_INTERP`; fall through.
   2. **Arch-filtered directory scan** of the canonical loader directories and
      their immediate multiarch subdirectories — catches multi-libc hosts the
      single `PT_INTERP` read misses; foreign-arch loaders are filtered out.
   3. **Hardcoded allowlist (fallback)** — the v1 paths, demoted to last resort.

   Dedup by canonical path via the existing `dedup_unseen` helper before any
   spawn.

2. **Identification** — classify each discovered loader **purely by its
   `--version` banner**, independent of which source produced the path,
   table-driven over a family descriptor list (so a third libc family is a
   one-row addition). The musl exit-status-ignored and glibc exit-127 →
   `{loader} /bin/true` quirks are preserved.

**gcompat now handled by banner classification, not a special case.** The
gcompat stub sits at the glibc loader path but prints the **musl** banner →
classified `Musl`. The "identity, not equivalence" rule (locked in the
"gcompat / equivalents" decision row) is now preserved *by construction*; the
v1 special-case exclusion code is gone.

### Namespace reservation

This round emits **glibc + musl only**. The refactor makes a variant-add
trivial, and the `LibcFlavor::Unknown` variant already parses inbound unknown
`libc.*` tags losslessly, **reserving the `libc.*` namespace** (`libc.uclibc`,
`libc.bionic`, …). uClibc-ng / Bionic are NOT actively probed now (YAGNI).

### Dependency

`PT_INTERP` is parsed with the **`elf` crate** (cole14/rust-elf), not goblin:
zero runtime deps, zero `unsafe`, dual MIT/Apache (matches `deny.toml`),
ELF-only scope (ISP). Pinned `elf 0.8`; `cargo tree -i elf` confirms zero
transitive deps.

### Known limitation — detect-env ≠ exec-env

Detection answers "what libc can *this host* run?". When the resolved binary
ultimately runs in a *different* namespace (distrobox/toolbox, a bind-mounted
container, install-here-run-there), that target namespace may provide a
different libc set than the one detected. OCX's normal `ocx exec` runs on the
same host/kernel, so the gap does not bite the common path. **Exec-time /
target-namespace detection is deferred to a separate ADR.**

### Updated rows

- **Host detection** (Concrete decisions locked): now "discovery-then-identify —
  `PT_INTERP` of a system binary ∪ arch-filtered directory scan ∪ hardcoded FHS
  allowlist (fallback), deduped by canonical path; banner-only table-driven
  identification. Linux-only."
- **NixOS host detection** (Risks): the prior mitigation ("Future: parse
  `/proc/self/exe` PT_INTERP for fallback") is now **implemented** — PT_INTERP
  discovery of a system binary finds the `/nix/store` loader on stock NixOS. The
  empty-set degrade remains only for minimal NixOS images with no readable
  dynamic loader.

## Links

- [`research_libc_detection_robustness.md`](./research_libc_detection_robustness.md) — detection mechanism v2 research (PT_INTERP / scan / allowlist; virtualization failure modes; elf-vs-goblin)
- [`research_libc_detection_methods.md`](./research_libc_detection_methods.md) — v1 detection research (first-wins / single-value fixes, disk-enumeration choice)
- [`research_libc_platform_differentiation.md`](./research_libc_platform_differentiation.md) — full research notes
- [`adr_cascade_platform_aware_push.md`](./adr_cascade_platform_aware_push.md) — sibling decision; `has_platform` strict-equality semantics referenced from there
- [`subsystem-oci.md`](../rules/subsystem-oci.md) — OCI subsystem rules
- [OCI image-index spec v1.1.1](https://github.com/opencontainers/image-spec/blob/v1.1.1/image-index.md)
- [image-spec issue #1216 — Platform-Specific OCI Artifacts](https://github.com/opencontainers/image-spec/issues/1216) (related discussion thread)
- [cargo-binstall `detect-targets`](https://github.com/cargo-bins/cargo-binstall/tree/main/crates/detect-targets) — host detection reference implementation
- [PEP 600 — manylinux](https://peps.python.org/pep-0600/) — prior art for subset/superset libc-version resolution
- [PEP 656 — musllinux](https://peps.python.org/pep-0656/) — prior art for musl detection

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-28 | mherwig + architect | Initial draft following ~5 rounds of design discussion + 2 parallel researcher passes + 1 architecture-explorer discovery |
| 2026-05-28 | architect + Phase 6 review panel | **Status: Accepted (user).** Amendments: (1) Subset matcher pseudocode corrected — Any candidate now matches every host, unifying publisher-declared agnostic and bare-manifest fallback cases without ambiguity; the initial draft accidentally forbade `Specific host_supports Any candidate` which would have broken static-musl + JAR install flows. (2) `HostCapabilities::os_features()` added to API Contract. (3) Quantified Impact "wrong-libc pulls eliminated" weakened to require publisher correctness. (4) Risks added for cascade `os_features` array ordering, publisher mis-tagging, and OCI issue #1237 character-set proposal. (5) Vendor-prefixed namespace explicitly considered and rejected with rationale. Driven by `worker-reviewer` (spec-compliance, 10 actionable), `worker-architect` (3 blockers + 5 strong recs), `worker-researcher` (1 new signal — issue #1237). |
| 2026-05-29 | builder (review-fix) | `LibcFlavor` gained an `Unknown(String)` variant (no longer `Copy`; `Ord`/`Hash` retained so `BTreeSet` iteration stays deterministic with `Unknown` sorting after the known unit variants). Added canonical `LibcFlavor::{os_feature_tag, from_os_feature_tag}` and a typed lenient `Feature::{Libc, Other}` model with `Feature::parse`; the forward (`os_features`) and reverse (`cached_libc_labels`) maps now route through these methods (single source of truth — the two independent literal tables are gone). `about`/`version` libc fields now emit the full `libc.*` tag (e.g. `["libc.glibc"]`), consistent with the resolver wire form. `to_platform_arg` renders features via the shared `normalize_os_features` helper instead of an inline sort+dedup. See "os.features interpretation is lenient". |
| 2026-05-31 | builder (detection v2) | **Detection mechanism v2 — discovery-then-identify** (see Amendment section). `HostCapabilities::detect` now discovers candidate loaders from `PT_INTERP` of a fixed system-binary allowlist (`elf` crate) ∪ an arch-filtered directory scan ∪ the hardcoded FHS allowlist (demoted to fallback), deduped by canonical path, then classifies each purely by `--version` banner via a table-driven `LIBC_FAMILIES` descriptor list. Fixes false-negatives on non-FHS hosts (NixOS, Gentoo Prefix, custom sysroots). gcompat → musl now falls out of banner classification (special-case exclusion removed). Still emits `Glibc`/`Musl` only; `libc.*` namespace reserved via `Unknown`. Added `elf 0.7.4` dependency (zero runtime deps, zero `unsafe`, dual MIT/Apache). Documented detect-env ≠ exec-env as a known limitation (future ADR). Wire format, subset matcher, and published-artifact contract unchanged. |
| 2026-05-28 | builder (Phase 4 implementation) | `Platform::matches` (strict-equality matcher) **deleted**; its single production caller at `oci/index.rs` swapped to `Platform::host_supports`; the `matches_*` unit tests removed. `Index::select` now returns the new `SelectResult::LibcMismatch { host_features, available }` variant (mapped to `PackageErrorKind::LibcMismatch` in `package_manager::tasks::resolve`) when the host detected a libc but no os+arch candidate satisfies subset matching. Within a matched tier, selection now prefers the most specific candidate (max `os_features` count the host satisfies), so a glibc host picks the `libc.glibc` entry over a co-present bare entry; equally specific matches still surface as `Ambiguous`. `From<&Platform> for native::Platform` stops serializing the RESERVED `features` field and normalizes `os_features` (sort + dedup); inbound `TryFrom` warn-and-drops populated `features`. |
| 2026-05-31 | review-fix (max-tier /swarm-review) | **Naming reconciliation** (no behavior change). The matcher shipped as `Platform::can_run` (not the `host_supports` name used in this changelog row above and the plan); `can_run` is canonical — it reads correctly as "host `can_run` candidate" at the `index.rs` call site. The select-result + error variant shipped as `SelectResult::FeatureMismatch` / `PackageErrorKind::FeatureMismatch` (not `LibcMismatch`): the matcher is generic over `os.features`, not libc-specific (libc is merely the first such feature), so the feature-neutral name is correct. Both renames are recorded here so the locked-decisions table (`can_run`, already correct) and the implementation agree; `subsystem-oci.md` `SelectResult` block updated to include the `FeatureMismatch` variant. Also: `elf` dependency bumped `0.7.4` → `0.8` (no CVE; the `minimal_parse`/`segments`/`PT_INTERP` API OCX uses is stable across the boundary). Rebased onto v0.3.3 base (the prior checkpoint had reverted the release). |
| 2026-07-05 | review-fix (r5) | **`os_version` fail-closed rule** (Finding #1). `can_run` now binds `os_version` on both sides and rejects a candidate whose declared `os_version` differs from the host's — `Platform::current()` never populates it, so a version-bearing candidate stays unselectable, preserving the deleted strict-`matches` behaviour until a version-range ADR supersedes it. Locked-decisions table gained an `os_version` matching row; four regression tests added. **Known-limitation row added** (Finding #5): `all_supported()` enumerates only empty-`os_features` platforms + `Any`, so `patch sync` without `--platform` silently skips libc-tagged companions; code fix deferred to keep the `can_run` invariant narrow. No wire-format or published-artifact change. |
| 2026-07-18 | amendment (unification follow-up) | **`os_version` axis deleted** by `adr_platform_model_unification.md`: the field is gone from `Platform::Specific`, so the 2026-07-05 fail-closed rule above is superseded (nothing left to match). `can_run` itself is superseded by `is_compatible`/`select_best` (unification D1). Forward-looking `os.version` mentions in this document (Option 1 pros, D5 "future path", Consequences "future axes") are dead — any version-range axis requires a new ADR and a new model field. Historical sections are left as written. |
