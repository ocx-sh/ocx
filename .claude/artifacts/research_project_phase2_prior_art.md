# Research: Phase 2 Prior Art — JCS, serde_repr, Atomic Writes, TOML Determinism

Scope: research for Phase 2 of `plan_project_toolchain.md` (schema + canonicalization). Feeds the stub → implement workers.

## Axis 1: RFC 8785 JCS Canonicalization Crates

Three candidates evaluated.

**`serde_json_canonicalizer` v0.3.2 (released 2026-02-03) — RECOMMENDED**
- ~369k downloads/month; used by 109 crates — highest-adoption JCS crate in the Rust ecosystem.
- API: `to_string(impl Serialize)`, `to_vec(...)`, `to_writer(...)`, `pipe(json_str: &str)` for raw JSON string input. `serde_json::Value` implements `Serialize` and works with all four.
- Explicitly uses `ryu-js` for IEEE 754 number serialization. This is the RFC 8785 § 3.2.2.3-mandated algorithm (ECMAScript `NumberToString`). Standard `ryu` produces different bit-representations for some edge-case doubles; `ryu-js` does not.
- Created specifically because `serde_jcs` had documented RFC divergences — the strongest maintenance signal available.
- Known footgun (not reachable for OCX): `serde_json` `arbitrary_precision` feature causes precision loss when canonicalizing. The declaration hash input consists only of strings and sorted arrays (no floats), so the footgun cannot fire.

**`serde_jcs` v0.2.0 (released 2026-03-25)**
- ~74k downloads/month (5× lower); 172 dependents but smaller than `serde_json_canonicalizer`.
- Multi-year maintenance gap (2022–2025). 0.2.0 updated to Rust 2024 edition but the changelog does not enumerate RFC compliance gaps fixed.
- Does not document use of `ryu-js`; number serialization algorithm unknown, making RFC compliance unverifiable without source inspection.
- Actively cited by `serde_json_canonicalizer` as the crate it was created to replace.

**`json-canonicalization`**: cyberphone (RFC author) reference port. Low Rust ecosystem adoption. Skip.

**Decision:** `serde_json_canonicalizer` wins on adoption trajectory, explicit `ryu-js` choice, and founding RFC-correctness motivation. Build the hash input as a `serde_json::Value` with sorted keys (belt-and-suspenders; JCS re-sorts anyway), pass to `to_string()`, SHA-256 the UTF-8 bytes, format as `sha256:<hex>`.

## Axis 2: `serde_repr` Versioning Pattern

`serde_repr` v0.1.20 (dtolnay, released 2025-03-04). Well-maintained.

**Unknown discriminant behavior:** `Deserialize_repr` rejects unknown values with a structured error: `"invalid value: {actual}, expected one of: 1"` (or `"expected 1 or 2"` with two variants, etc.). No silent fallback unless `#[serde(other)]` is explicitly placed on a catch-all variant. Omitting `#[serde(other)]` gives exactly the loud-failure-on-unknown-version behavior the plan requires.

**Idiomatic `LockVersion`:**

```rust
#[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, PartialEq, Debug, Clone, Copy)]
#[repr(u8)]
pub enum LockVersion { V1 = 1 }
```

Adding `V2 = 2` later is a one-liner — existing v1 files still deserialize correctly. `#[non_exhaustive]` is intentionally absent: the `arch-principles.md` convention says "omit `#[non_exhaustive]` on internal non-error enums" and `LockVersion` is an on-disk discriminant consumed only within the OCX workspace. `quality-rust.md` already documents this exact pattern under "Version Enum via `serde_repr`".

## Axis 3: Atomic TOML Lock File Writes

**The existing OCX pattern handles the cross-filesystem gotcha correctly.**

Reference: `ProfileManifest::save` at `crates/ocx_lib/src/profile.rs:160-188`:

```rust
let tmp = tempfile::NamedTempFile::new_in(parent)?;  // parent = path.parent()
std::fs::write(tmp.path(), bytes)?;
tmp.persist(path)?;                                   // rename(2) on Unix
```

`NamedTempFile::new_in(parent)` creates the temp file in the same directory as the destination, guaranteeing the same filesystem mount. `rename(2)` fails across mounts, but `persist()` returns a recoverable `PersistError` — which cannot occur when `parent == path.parent()`. Identical to Cargo's pattern in `src/cargo/util/config/mod.rs`.

**One gap to close:** `std::fs::write` does not `fsync()` before the rename. For `ocx.lock` — a committed VCS artifact where durability across power-loss matters — add `tmp.as_file().sync_data()?` between the write and `persist()`. `sync_data()` flushes data without filesystem metadata (faster than `sync_all()`).

**Recommended refactor:** Extract a shared helper `utility::fs::atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()>` in `crates/ocx_lib/src/utility/fs.rs` that implements `new_in(parent)` + write + `sync_data` + `persist`. Both `ProfileManifest::save` and `ProjectLock::save` should call it. Upstream to the Utility Catalog in `arch-principles.md`. (For Phase 2 we can inline the pattern directly in `project::lock` and defer the extraction to a follow-up refactor if scope creep is a concern — the plan does not mandate the catalog upstream.)

## Axis 4: TOML Serializer Determinism

**Use `toml::to_string_pretty()`. Do not use `toml_edit` for lock generation.**

`toml` crate (v0.8+, actively maintained): serializes named struct fields in definition order (stable, because Rust struct field order is stable across compilations) and `BTreeMap` fields in ascending key order (because `BTreeMap::iter()` yields entries sorted by key and the serde serializer traverses in order). All `ProjectLock` structs in Phase 2 use `BTreeMap` — determinism guaranteed provided no `HashMap` creeps in.

`toml_edit`: format-preserving round-trip crate. Preserves insertion order; does NOT sort keys. Wrong tool for writing a fresh canonical lock file.

**Non-determinism checklist (verify at stub review):**
- Any `HashMap<K, V>` anywhere in lock structs → non-deterministic output. Must be `BTreeMap`.
- `toml::to_string_pretty()` emits a trailing newline. Stable and correct per POSIX text file convention.
- Inline vs. standard table choice is heuristic-driven. Nested structs serialize as standard (multi-line) tables with `to_string_pretty()` by default — no action needed.

## Recommendations Summary

| Axis | Decision |
|---|---|
| JCS crate | `serde_json_canonicalizer` v0.3.2 — add to `crates/ocx_lib/Cargo.toml` |
| Hash input | `serde_json::Value` with sorted keys → `serde_json_canonicalizer::to_string` → SHA-256 → `sha256:<hex>` |
| Version enum | `Serialize_repr`/`Deserialize_repr` on `#[repr(u8)] pub enum LockVersion { V1 = 1 }`; no `#[non_exhaustive]`, no `#[serde(other)]` |
| Atomic write | Clone `profile.rs:160-188` pattern into `project::lock::save`; add `tmp.as_file().sync_data()?` before `persist()`. Helper extraction deferrable. |
| TOML serializer | `toml::to_string_pretty()`; all maps must be `BTreeMap` |

## Sources

- [serde_json_canonicalizer crate (GitHub)](https://github.com/evik42/serde-json-canonicalizer) — implementation, `ryu-js` usage, RFC motivation
- [serde_json_canonicalizer docs.rs](https://docs.rs/serde_json_canonicalizer/latest/serde_json_canonicalizer/) — API shape
- [lib.rs: serde_json_canonicalizer](https://lib.rs/crates/serde_json_canonicalizer) — download stats, maintenance
- [lib.rs: serde_jcs](https://lib.rs/crates/serde_jcs) — download stats, maintenance history
- [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785) — § 3.2.2.3: IEEE 754 serialization requirement
- [dtolnay/serde-repr](https://github.com/dtolnay/serde-repr) — unknown discriminant behavior, `#[serde(other)]` fallback
- [docs.rs/serde_repr](https://docs.rs/serde_repr/latest/serde_repr/) — derive reference
- [NamedTempFile::persist docs](https://docs.rs/tempfile/latest/tempfile/struct.NamedTempFile.html) — cross-filesystem limitation
- [users.rust-lang.org: atomic file writes](https://users.rust-lang.org/t/how-to-write-replace-files-atomically/42821) — rationale for same-directory pattern
- [docs.rs/toml](https://docs.rs/toml/latest/toml/) — serializer behavior
- [toml v0.9 blog post](https://epage.github.io/blog/2025/07/toml-09/) — crate evolution (no determinism regressions noted)
