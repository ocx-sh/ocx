# Plan: Adopt underscore build separator (ADR)

## Overview

**Status:** Draft
**Author:** Claude
**Date:** 2026-03-12
**Beads Issue:** N/A
**Related PRD:** N/A
**Related ADR:** [adr_version_build_separator.md](adr_version_build_separator.md)

## Objective

Switch the build separator from `+` to `_` at two layers:

1. **`Identifier` (OCI boundary)** — normalize `+` → `_` in tags at parse time, so every `Identifier` produces valid OCI tags by construction. This is the earliest point where user input enters the system.
2. **`Version` (semantic layer)** — accept both `+` and `_` in `parse()`, always emit `_` in `Display`.

The `Identifier` normalization is essential because not all tags are version strings — a user could type `cmake:3.28.1+20260216` on the CLI, and the `Identifier` parser extracts `3.28.1+20260216` as the raw tag. Without normalization here, the `+` would reach the OCI transport layer and be rejected by registries.

## Scope

### In Scope

- `Identifier` tag normalization (`+` → `_` at parse time)
- `Version` regex and `Display` implementation
- Unit tests in `version.rs` and `identifier.rs`
- Comments in `cascade.rs` test code
- Website documentation (`command-line.md`, `faq.md`, `user-guide.md`)
- AI-facing documentation (`.claude/rules/`, `.agents/artifacts/`)

### Out of Scope

- Acceptance tests (already use `Version` structs / `new_build()` — no `+` string literals)
- OCI media types (`application/vnd.oci.image.manifest.v1+json` — this `+` is MIME syntax, not version syntax)
- `tag.rs`, `cascade.rs` logic (operate on `Version` structs, not strings)

## Technical Approach

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| Normalize `+` → `_` in `Identifier` tag at parse time | Earliest boundary — catches all user input before it reaches OCI transport or `Version::parse()`. Without this, `cmake:3.28.1+20260216` on the CLI would produce an invalid OCI reference |
| Accept `[_+]` in `Version` regex, emit `_` only | Defense in depth — `Version::parse()` may receive tags from sources other than `Identifier` (e.g., index JSON files, tests). Tolerant input, strict output |
| No migration tooling | No production data uses `+` tags yet; this is a pre-release fix |
| Update comments in cascade.rs | Keep code comments consistent with the canonical format |

## Implementation Steps

### Phase 1: Identifier normalization (OCI boundary)

- [ ] **Step 1.1:** Add `+` → `_` normalization in `Identifier` tag handling
  - File: `crates/ocx_lib/src/oci/identifier.rs`
  - Where: `split_tag()` function (line 313) or in `parse()` after `split_tag` returns
  - Change: Replace `+` with `_` in the extracted tag string before storing it
  - This ensures all downstream code — `Version::parse()`, OCI transport, filesystem paths — sees `_` only
  - Note: `clone_with_tag()` should also normalize, since callers may pass user-provided strings

- [ ] **Step 1.2:** Add `Identifier` tests for `+` normalization
  - File: `crates/ocx_lib/src/oci/identifier.rs` (test module)
  - Test cases:
    - `"cmake:3.28.1+20260216".parse()` → tag is `Some("3.28.1_20260216")`
    - `"cmake:3.28.1_20260216".parse()` → tag is `Some("3.28.1_20260216")` (already canonical)
    - `"test:5000/repo:1.0+build".parse()` → tag is `Some("1.0_build")` (with registry port)
    - Display roundtrip: parse with `+`, display, reparse — identical
    - `clone_with_tag("3.28.1+b1")` → tag is `Some("3.28.1_b1")`

### Phase 2: Version type (semantic layer)

- [ ] **Step 2.1:** Update `Version::parse()` regex to accept both `_` and `+`
  - File: `crates/ocx_lib/src/package/version.rs` line 160
  - Change: `(\+([0-9a-zA-Z]+))` → `([_+]([0-9a-zA-Z]+))`
  - The regex group structure stays the same (group 9 = build value)

- [ ] **Step 2.2:** Update `Version::Display` to emit `_` instead of `+`
  - File: `crates/ocx_lib/src/package/version.rs` line 302
  - Change: `format!("+{}", build)` → `format!("_{}", build)`

### Phase 3: Version unit tests

- [ ] **Step 3.1:** Update existing test expectations in `version.rs`
  - File: `crates/ocx_lib/src/package/version.rs`
  - `test_version_is_rolling` (line 409): `parse("1.2.3+20260216")` → keep as tolerance test, add assertion that `to_string()` returns `"1.2.3_20260216"`
  - `test_version_is_rolling` (line 411): same for `"1.2.3-alpha+20260216"`

- [ ] **Step 3.2:** Add new test `test_build_separator_normalization`
  - File: `crates/ocx_lib/src/package/version.rs`
  - Test cases:
    - `parse("1.2.3_build")` succeeds and `to_string()` == `"1.2.3_build"`
    - `parse("1.2.3+build")` succeeds and `to_string()` == `"1.2.3_build"` (tolerance)
    - `parse("1.2.3_build") == parse("1.2.3+build")` (equality)
    - `parse("1.2.3-alpha_build")` and `parse("1.2.3-alpha+build")` both normalize to `"1.2.3-alpha_build"`
    - Round-trip: `parse(v.to_string()) == Some(v)` for all constructible build versions

- [ ] **Step 3.3:** Add test `test_version_display_uses_underscore`
  - File: `crates/ocx_lib/src/package/version.rs`
  - Verify `new_build(1, 2, 3, "b1").to_string()` == `"1.2.3_b1"`
  - Verify `new_prerelease_with_build(1, 2, 3, "alpha", "b1").to_string()` == `"1.2.3-alpha_b1"`

### Phase 4: Code comments

- [ ] **Step 4.1:** Update cascade.rs test comments
  - File: `crates/ocx_lib/src/package/cascade.rs`
  - Lines 389, 391: `3.28.2+b1` → `3.28.2_b1`
  - Lines 514, 515: `3.27.0+b1`, `3.28.0+b1` → `3.27.0_b1`, `3.28.0_b1`
  - Line 705: `3.28.1+b1` → `3.28.1_b1`
  - Line 758: `3.28.1+b1` → `3.28.1_b1`
  - Line 775: `4.0.0+b1` → `4.0.0_b1`

### Phase 5: Website documentation

- [ ] **Step 5.1:** Update `command-line.md`
  - File: `website/src/docs/reference/command-line.md`
  - Line 425: `cmake:3.28.1+20260216120000` → `cmake:3.28.1_20260216120000`
  - Line 431: `cmake:3.28.1+20260216120000` → `cmake:3.28.1_20260216120000` (both occurrences)

- [ ] **Step 5.2:** Add FAQ section for versioning / build separator
  - File: `website/src/docs/faq.md`
  - Add a new `## Versioning` section (before the existing `## macOS` section)
  - Add `### Build Separator {#versioning-build-separator}` entry explaining:
    - The problem: OCI tags don't allow `+`, but semver uses `+` for build metadata
    - The solution: ocx uses `_` as the build separator (follows Helm convention)
    - The grammar: `{major}[.{minor}[.{patch}[-{prerelease}][_{build}]]]`
    - Tolerant input: typing `+` works and auto-normalizes to `_`
    - Link to the OCI Distribution Spec tag grammar and Helm convention
    - Link back to the user-guide versioning section for the full tag hierarchy
  - Style: follow FAQ documentation rules — `:::info` for Helm analogy, link to [OCI Distribution Spec][oci-dist-spec] and [Helm OCI][helm-oci]
  - Add reference-style links at bottom of faq.md

- [ ] **Step 5.3:** Update user-guide.md versioning section cross-references
  - File: `website/src/docs/user-guide.md`
  - In the existing `:::details Why _ instead of semver's +?` block (lines 234-238):
    - Replace `:::details` with `:::tip` (this is actionable advice, not optional depth)
    - Add a link to the new FAQ entry: `See [Build Separator][faq-build-separator] in the FAQ for the full rationale.`
  - Add link definition at bottom: `[faq-build-separator]: ./faq.md#versioning-build-separator`

### Phase 6: AI-facing documentation (rules & artifacts)

- [ ] **Step 6.1:** Update `.claude/rules/documentation.md`
  - Line 67: `+build` → `_build`

- [ ] **Step 6.2:** Update `.claude/rules/cli-commands.md`
  - Line 308: `cmake:3.29+build.1` → `cmake:3.29_build.1`

- [ ] **Step 6.3:** Update `.agents/artifacts/` planning docs (comments only — these are historical but should stay consistent)
  - `plan_cascade_hardening.md`: all `+b1` → `_b1` in test case descriptions
  - `plan_cascade_refactoring.md`: line 111 `3.28.2+b1` → `3.28.2_b1`, line 114 `3.28.1+b1` → `3.28.1_b1`
  - `adr_cascade_platform_aware_push.md`: `+build` references → `_build`
  - `adr_version_build_separator.md`: No change — this ADR documents the transition itself and should preserve both forms

### Phase 7: Verification

- [ ] **Step 7.1:** `cargo fmt`
- [ ] **Step 7.2:** `cargo clippy --workspace`
- [ ] **Step 7.3:** `cargo nextest run --workspace` — all unit tests pass
- [ ] **Step 7.4:** `task test` — all acceptance tests pass
- [ ] **Step 7.5:** Spot-check: `cargo test -p ocx_lib test_build_separator` — new test passes

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `crates/ocx_lib/src/oci/identifier.rs` | Modify | Normalize `+` → `_` in tags at parse time + `clone_with_tag()`; add tests |
| `crates/ocx_lib/src/package/version.rs` | Modify | Regex: accept `[_+]`; Display: emit `_`; add tests |
| `crates/ocx_lib/src/package/cascade.rs` | Modify | Update comments only (7 lines) |
| `website/src/docs/reference/command-line.md` | Modify | Update 2 `+` examples to `_` |
| `website/src/docs/faq.md` | Modify | Add `## Versioning` section with build separator FAQ entry |
| `website/src/docs/user-guide.md` | Modify | Update details→tip callout, add cross-link to FAQ |
| `.claude/rules/documentation.md` | Modify | `+build` → `_build` (1 line) |
| `.claude/rules/cli-commands.md` | Modify | `+build.1` → `_build.1` (1 line) |
| `.agents/artifacts/plan_cascade_hardening.md` | Modify | `+b1` → `_b1` in test descriptions |
| `.agents/artifacts/plan_cascade_refactoring.md` | Modify | `+b1` → `_b1` (2 lines) |
| `.agents/artifacts/adr_cascade_platform_aware_push.md` | Modify | `+build` → `_build` (3 lines) |

## Dependencies

None — this is a self-contained change with no new packages or services.

## Testing Strategy

### Unit Tests

| Component | Test Cases | Status |
|-----------|------------|--------|
| `Identifier::parse()` | `+` in tag normalized to `_`; `_` preserved; registry port not affected | Pending |
| `Identifier::clone_with_tag()` | `+` in tag normalized to `_` | Pending |
| `Identifier` roundtrip | parse with `+`, display, reparse — identical | Pending |
| `Version::parse()` | `_` input accepted, `+` input accepted, both produce same `Version` | Pending |
| `Version::Display` | Always emits `_`, never `+` | Pending |
| Round-trip | `parse(v.to_string()) == Some(v)` for build versions | Pending |
| Ordering | Build versions with `_` sort identically to previous `+` behavior | Pending (existing tests cover) |

### Integration Tests

| Scenario | Expected Outcome | Status |
|----------|------------------|--------|
| Acceptance tests (unchanged) | All pass — they use `new_build()` not string literals | Pending |
| Cascade push with build tag | Registry receives valid OCI tag with `_` | Pending (existing acceptance tests) |

## Rollback Plan

1. Revert the commit (single commit, all changes)
2. `cargo test` to verify revert is clean

## Risks

| Risk | Mitigation |
|------|------------|
| Existing local index files contain `+` tags | `Version::parse()` accepts `+` input — old data reads fine |
| Users have scripts that grep for `+` in version output | Breaking change documented; `_` convention was already in user-guide.md |
| OCI spec eventually adds `+` support | We accept `+` on input already — migration would be output-only cosmetic change |

## Checklist

### Before Starting

- [x] ADR written and reviewed
- [x] No external dependencies needed
- [ ] Branch created from main

### Before PR

- [ ] All unit tests passing (`cargo nextest run --workspace`)
- [ ] `cargo clippy --workspace` clean
- [ ] `cargo fmt` applied
- [ ] Acceptance tests passing (`task test`)
- [ ] Documentation updated (website + rules)

### Before Merge

- [ ] Code review approved
- [ ] `task verify` passes

## Notes

- **Two-layer normalization**: `Identifier` normalizes at the OCI boundary (catches CLI input), `Version` normalizes at the semantic layer (catches index data, test strings). Both are needed for defense in depth.
- The `+` in OCI media types (`application/vnd.oci.image.manifest.v1+json`) is MIME `type+suffix` syntax — completely unrelated to version build separators. Do not change these.
- The `adr_version_build_separator.md` itself should NOT be updated — it documents the transition and intentionally shows both `+` and `_`.
- Cascade logic in `cascade.rs` operates entirely on `Version` structs via `new_build()` — only comments reference the string form.
- `Identifier` stores tags as raw strings — it does not parse them as versions. The `+` → `_` replacement is a simple string operation, not version-aware. This is intentional: even non-version tags with `+` are invalid OCI tags.

---

## Progress Log

| Date | Update |
|------|--------|
| 2026-03-12 | Plan created |
