# Plan: Wasm platform support + supported-pairs validation

## Status

- **Plan:** plan_wasm_platform_pairs
- **Active phase:** done — implemented, reviewed (opus round 1: 1 docs finding fixed; Codex terra gate: 1 finding filtered by scope), merged ff onto soraka
- **Step:** awaiting /finalize
- **Last update:** 2026-08-21 (after 1f94857f: feat(oci): add wasm platforms (wasip1/wasm, wasip2/wasm) and enforce supported platform pairs)

## Context

A co-worker wants to distribute WebAssembly files through ocx. Research (CNCF Wasm WG OCI artifact layout; Go GOOS/GOARCH; deprecated Docker `wasi/wasm` convention) settled on the standardized platform values `wasip1/wasm` (modules) and `wasip2/wasm` (components). Discussion with the owner also surfaced that ocx validates os and arch segments independently — no (os, arch) conjunction check exists, so e.g. `wasip1/amd64` would parse once wasm variants land. Agreed scope:

1. Add OS variants `Wasip1`, `Wasip2` and arch variant `Wasm` to the closed platform enums.
2. Add a supported-(os, arch)-pairs array, validated hard at the two parse choke points.
3. `variant` + `os_features` stay untouched (open strings, no new lints — explicitly decided).
4. Native platform holds (linux/arm, riscv64, ppc64le, s390x, freebsd, 386, js/wasm) stay OUT — doctrine: a native label requires ocx to compile + test there; wasm is label-only by nature.
5. Wasm targets get **no automatic binary scan** (owner decision): empty binaries claim, files are data artifacts.

Supported pairs after this change (all 6 current real pairs + 2 wasm):
`linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64, windows/arm64, wasip1/wasm, wasip2/wasm`.
Note: windows/arm64 already parses and works today (both variants exist, no pair check; ocx ships `aarch64-pc-windows-msvc`) — the array formalizes it; the pair check therefore breaks nothing that parses today.

Classification: **feature** (workflow-feature).

## Key facts from exploration (verified)

- Enums + recipe: `crates/ocx_lib/src/oci/platform/operating_system.rs`, `architecture.rs` — module docs document the extension recipe (variant + `Display` + `FromStr` + `VARIANTS` + both native conversions). `Ord` derives from declaration order and a `variants_is_sorted` test pins it — append new variants last.
- `native::Arch::Wasm` exists (oci-client fork). **`native::Os` has NO wasip variants** → map via `native::Os::Other("wasip1".to_string())` / `Other("wasip2")`; `TryFrom<native::Os>` must match `Os::Other(s)` for those two strings before the generic reject. Confirm during build that `native::Os::Other(s)` serializes as the bare string in manifest JSON (expected; existing `Other("custom")` test relies on it).
- Only two construction choke points build `Platform::Specific` from raw parts: `Platform::FromStr` (platform.rs:536-594) and `TryFrom<native::Platform>` (platform.rs:745-803). Everything else destructures. Pair validation in those two places is complete coverage. Read path stays tolerant: `candidate_from_descriptor` (platform.rs:142) already skips unrepresentable descriptors — a foreign `linux/wasm` entry is skipped, never a hard error.
- Roundtrip tests `serde_roundtrip_all_supported_combinations` (platform.rs:1901) and `native_roundtrip_all_supported_combinations` (platform.rs:2090) iterate the full VARIANTS cross product — must be rewritten to iterate the pairs array.
- bin_scan (`crates/ocx_lib/src/package/bin_scan.rs`): `is_windows_target()` (line 236) → extension allowlist; else Unix exec-bit branch. A wasm target would wrongly fall into the Unix branch.
- Starlark: `script/os_value.rs:51-57` + `arch_value.rs:48-53` `starlark_name()` exhaustive matches — compiler forces new arms. Parity tests are generic over VARIANTS, no edit needed.
- `ocx_schema` does not enumerate platform values — **no `task schema` regeneration needed**.
- Exit codes: `PlatformError` already classifies to `DataError` (65); `--platform` flag path goes through clap → 64. No new classification work.

## Implementation steps

### 1. Enum variants (mechanical, per module-doc recipe)

- `oci/platform/architecture.rs`: add `Wasm` (after `Arm64`), remove from commented-out list; `Display` → `"wasm"`; `FromStr` → `"wasm"`; `VARIANTS`; `From<Architecture> for native::Arch` → `native::Arch::Wasm`; `TryFrom<native::Arch>` accept `Wasm`. Update `fromstr_rejects_unsupported_oci_values` (drop `"wasm"` row) and `native_rejects_unsupported` (drop `native::Arch::Wasm` row); host `current()` unchanged (wasm never a detected host).
- `oci/platform/operating_system.rs`: add `Wasip1`, `Wasip2` (after `Windows`); `Display`/`FromStr` → `"wasip1"`/`"wasip2"`; `VARIANTS`; `From<OperatingSystem> for native::Os` → `Os::Other("wasip1"/"wasip2")`; `TryFrom<native::Os>` match `Os::Other(s)` for the two strings ahead of the generic reject. `current()` unchanged.

### 2. Supported-pairs validation

In `oci/platform.rs`:
- `pub const SUPPORTED_PAIRS: &[(OperatingSystem, Architecture)]` — the 8 pairs above, with a rationale comment (native pairs = ocx ships + tests there; wasip pairs = distribution labels, ocx never runs on them).
- New `PlatformErrorKind::UnsupportedPair { os, arch }` in `oci/platform/error.rs` — lowercase message listing the supported pairs (ERR-05 style, mirrors existing `UnsupportedOs`/`UnsupportedArch` shape). Classification inherits `DataError` via existing `PlatformError` impl.
- Call a shared `validate_pair(os, arch)` from `Platform::FromStr` (after segment parse) and `TryFrom<native::Platform>` (after os/arch conversion). No other site constructs from raw parts.

### 3. bin_scan: wasm targets = no auto-scan

`package/bin_scan.rs`: add `is_wasm_target()` (`os: Wasip1 | Wasip2`) checked before the Windows/Unix branch — wasm target yields an empty binaries claim deterministically on every host (`host_can_scan` stays true; the skip is convention, not host limitation). Comment states the decision (data artifacts; entrypoints declared explicitly in metadata if needed; extension scan addable later). During build, check how `create` reports an empty binaries claim (warn vs silent) and keep that behavior — no new warning machinery.

### 4. Starlark surface

`script/os_value.rs` / `arch_value.rs`: new `starlark_name()` arms — `"Wasip1"`, `"Wasip2"`, `"Wasm"` (PascalCase per parity test). Namespace building iterates VARIANTS, so exposure is automatic.

### 5. Tests

Unit (in the touched modules):
- `Platform::from_str`: accepts `wasip1/wasm`, `wasip2/wasm`; rejects `wasip1/amd64`, `linux/wasm`, `windows/wasm` with `UnsupportedPair`; error message lists pairs.
- Rewrite both `*_all_supported_combinations` roundtrip tests to iterate `SUPPORTED_PAIRS` (serde + native), covering the new pairs automatically.
- `TryFrom<native::Platform>`: `Os::Other("wasip1")` + `Arch::Wasm` → `Platform::Specific{Wasip1, Wasm}`; `Os::Other("junk")` still rejected.
- `is_compatible`: add truth-table rows — wasm offer vs native host required → false; wasm required (explicit `--platform`) vs wasm offer → true.
- bin_scan: wasm target on a tree with exec-bit + `.wasm` files → empty claim.
- Lock: one `validate_canonical_platform_keys` case with a `wasip1/wasm` key (round-trips canonical).

Acceptance (pytest, `test/tests/`):
- `--platform wasip1/amd64` on any platform-taking command exits 64 (clap parse path) — closes the pre-existing gap (no acceptance test rejects an unsupported platform value at all); no fixture needed.
- End-to-end: `ocx package create -p wasip1/wasm` on a dir with a dummy `.wasm` file → push to test registry → `install --platform wasip1/wasm` fetches it (pattern: `test_cross_platform_materialize.py`). If the fixture cost balloons during build, builder may descope to create+push assertions and flag it.

### 6. Documentation surfaces

- `website/src/docs/reference/platforms.md` (canonical grammar page, lines 19-33): extend `os`/`arch` EBNF sets; add a supported-pairs table with the two-tier note — native pairs (ocx runs + CI-tested) vs wasm pairs (distribution labels; ocx selects them only via explicit `--platform`, never host detection).
- `website/src/docs/authoring/multi-platform.md`: one short paragraph/example for wasm publishing (`-p wasip1/wasm`), noting no binaries claim.
- `crates/ocx_cli/src/options/platform.rs` doc comment: add `wasip1/wasm` illustrative example (respect help gates: ASCII, short lines, no internal refs).
- `.claude/rules/subsystem-oci.md:503`: update `Supported:` line to the 8 pairs + pairs-array pointer.
- `.claude/rules/product-context.md` "Platform support" line: same update.
- Changelog = commit subject (never edit CHANGELOG.md): `feat(oci): add wasm platforms (wasip1/wasm, wasip2/wasm) and enforce supported platform pairs`.

### 7. Out of scope (explicit)

- No `variant`/`os_features` validation or lints (decided).
- No new native platforms (linux/arm, riscv64, ppc64le, s390x, freebsd, 386, js/wasm) — on-hold doctrine.
- No host detection for wasm; no libc work (`libc_lint` already no-ops for non-linux).
- No CNCF wasm OCI artifact *format* (media types, component config) — ocx packages stay ocx packages; only the platform vocabulary aligns.
- No fork (`external/rust-oci-client`) changes — `Os::Other` mapping suffices.
- No schema regeneration (verified not needed).

## Execution shape

Single subsystem-centered change, ~10 files, design fully settled → one builder (opus per model policy: wire-format-adjacent + error semantics), contract-first TDD per project convention; review via standard review-fix loop with Codex terra gate. Worktree branch off `soraka` per parallel-agent convention.

## Verification

1. `task rust:verify` during the loop; final gate `task verify --force` (never piped).
2. Targeted: `cargo nextest run -p ocx_lib platform`, the new pytest tests via `cd test && uv run pytest tests/test_platform_pairs*.py -v` (name TBD by builder).
3. Red-then-green: pair-rejection unit test must fail before `validate_pair` lands (regression-test discipline); prove the acceptance 64-exit test red by running it against a pre-change binary (stale `test/bin/ocx` pitfall: rebuild with `--features ocx/__testing` and copy).
4. Manual smoke: `ocx package create -p wasip1/wasm` on a scratch dir; `--platform wasip1/amd64` exits 64 from CLI, and a hand-written lock key `wasip1/amd64` fails load with exit 65.
