# ADR: Use underscore as build separator in version tags

## Metadata

**Status:** Proposed
**Date:** 2026-03-12
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** api
**Supersedes:** N/A
**Superseded By:** N/A

## Context

OCX versions use a semver-inspired grammar:

```
{major}[.{minor}[.{patch}[-{prerelease}][+{build}]]]
```

The `+` character separates the build identifier (e.g., `1.2.3+20260216`). Build-tagged versions are immutable by convention and anchor the cascade algebra — rolling tags (`1.2.3`, `1.2`, `1`, `latest`) cascade from them.

**The problem:** The OCI Distribution Specification defines the tag grammar as `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`. The `+` character is invalid. Any version with a build identifier (the most important kind for cascade) cannot be pushed as an OCI tag. This is a correctness bug — `Version::to_string()` produces unpushable strings.

This is a known ecosystem-wide issue ([distribution-spec#154](https://github.com/opencontainers/distribution-spec/issues/154), open since 2020, unresolved). The `+` encodes as a space in `application/x-www-form-urlencoded`, making it unsafe in URLs even if registries accepted it.

## Decision Drivers

- **OCI compliance** — tags must be valid per the distribution spec; this is non-negotiable
- **Simplicity** — one representation, not two; no encoding/decoding layer
- **Unambiguity** — the build separator must not collide with other grammar tokens (`.` for components, `-` for pre-release)
- **Ecosystem precedent** — Helm, Rancher, and ArgoCD already normalize `+` → `_` for OCI tags
- **OCX is not semver** — the documentation already calls it "semver-inspired"; there is no obligation to use `+`

## Considered Options

### Option 1: Internal `+`, external `_` (encoding layer)

**Description:** Keep `+` in the `Version` struct and `Display` impl. Add `to_tag()` / `parse_tag()` methods that translate `+` ↔ `_` at OCI boundaries.

| Pros | Cons |
|------|------|
| Internal model stays "pure semver" | Two representations to keep in sync |
| Minimal change to existing code | Every OCI boundary needs to remember to use `to_tag()` not `to_string()` |
| Familiar to semver purists | Bugs when the wrong format leaks to the wrong layer |
| | Users see `+` in CLI output but `_` in registry — confusing |

### Option 2: Native `_` everywhere

**Description:** Replace `+` with `_` as the build separator in parsing, display, storage, and documentation. `Version` never contains or emits `+`.

| Pros | Cons |
|------|------|
| One representation — zero confusion | Diverges from semver spec |
| Valid OCI tags by construction | Existing users of `+` tags need migration (none in production yet) |
| No translation layer, no boundary bugs | `_` is less visually distinct than `+` |
| `_` is unambiguous: `-` = prerelease, `_` = build, `.` = component | |
| Follows Helm's established OCI convention | |

### Option 3: Drop build identifier from tags entirely

**Description:** Remove the build identifier from the tag grammar. Use OCI annotations (`org.opencontainers.image.version`) or manifest labels to store build metadata. Tags are `1.2.3`, `1.2.3-rc1`, etc.

| Pros | Cons |
|------|------|
| Tags are maximally simple | Destroys the cascade anchor — build-tagged versions are the foundation of rolling releases |
| No separator debate | Cannot distinguish two builds of `1.2.3` by tag alone |
| | Annotations are not universally supported or queryable via `list_tags` |
| | Breaks the core immutability convention |

## Decision Outcome

**Chosen Option:** Option 2 — Native `_` everywhere

**Rationale:**

OCX is not semver. The version grammar is a convention for enabling cascade publishing, not a standards-compliance exercise. Using `_` natively:

1. Eliminates the correctness bug (invalid OCI tags) at the source
2. Avoids an encoding/decoding layer that would be a perpetual source of bugs
3. Maintains a single canonical representation across CLI output, registry tags, local index, and documentation
4. Uses an unambiguous character that doesn't collide with existing grammar tokens

Option 1 (encoding layer) was rejected because dual representations violate KISS. Every place that converts `Version` to a string becomes a potential bug if it uses `Display` instead of `to_tag()`. Option 3 was rejected because it would destroy the cascade algebra — build-tagged versions are the immutable anchors that rolling tags cascade from.

### Consequences

**Positive:**
- All version strings are valid OCI tags by construction
- Single representation eliminates an entire class of encoding bugs
- Grammar is fully unambiguous: `.` (component), `-` (prerelease), `_` (build)

**Negative:**
- Users familiar with semver will notice the `_` instead of `+`
- Any existing documentation or examples using `+` must be updated

**Risks:**
- If OCI spec eventually adds `+` support, we'd have a convention mismatch. Mitigation: we already accept `+` as input (see below), so migration would be cosmetic only.

**Tolerant input parsing:**
- `Version::parse()` accepts both `_` and `+` as the build separator on input
- The canonical output is always `_` — `Display`, serialization, and OCI tags all emit `_`
- This means `ocx install cmake:3.28.1+20260216` works, but the stored/displayed tag is `3.28.1_20260216`
- Users coming from semver get the right behavior without friction

## Technical Details

### Grammar

```
version  = major [ "." minor [ "." patch [ prerel ] [ build ] ] ]
major    = "0" | non-zero *digit
minor    = "0" | non-zero *digit
patch    = "0" | non-zero *digit
prerel   = "-" 1*alphanumeric    ; e.g., -alpha, -rc1
build    = "_" 1*alphanumeric    ; e.g., _20260216, _build42
```

All tokens are valid OCI tag characters. The full version string matches `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`.

### Version hierarchy (unchanged)

```
1.2.3-alpha_B < 1.2.3-alpha < 1.2.3_B < 1.2.3 < 1.2 < 1
 (build)       (rolling pre)  (build)   (roll)  (roll) (roll)
```

### Cascade example

Publishing `cmake:3.28.1_20260216` with `--cascade`:

```
3.28.1_20260216  →  pushed (immutable anchor)
3.28.1           →  cascaded (rolling patch)
3.28             →  cascaded (rolling minor)
3                →  cascaded (rolling major)
latest           →  cascaded (if no blocker)
```

### Changes required

**`identifier.rs` (OCI boundary — critical):**
- Normalize `+` → `_` in tags at parse time in `split_tag()` or `parse()`
- Also normalize in `clone_with_tag()` since callers may pass user-provided strings
- This is the earliest point where user CLI input enters the system; without this, `cmake:3.28.1+20260216` would produce an invalid OCI reference that registries reject

**`version.rs` (semantic layer — defense in depth):**
- Regex: accept both `_` and `+` as build separator in `parse()`
- `Display`: always emit `_`
- Normalizes on input: `parse("1.2.3+build")` and `parse("1.2.3_build")` produce identical `Version` values

**`tag.rs`:** No change — delegates to `Version::parse()`

**`cascade.rs`:** No change — operates on `Version` structs, not strings

**`package_push.rs`:** No change — calls `Version::parse()` on the tag string

**Documentation:** Update `command-line.md` examples and AI-facing rules from `+` to `_`

**Tests:** Update string literals in `version.rs` tests; add `identifier.rs` normalization tests

## Implementation Plan

1. [ ] Normalize `+` → `_` in `Identifier` tag parsing and `clone_with_tag()`
2. [ ] Add `Identifier` normalization tests
3. [ ] Update `Version` regex to accept both `_` and `+` as build separator
4. [ ] Update `Version::Display` to always emit `_`
5. [ ] Update unit tests in `version.rs`
6. [ ] Update cascade test comments
7. [ ] Update documentation (`command-line.md`, rules, artifacts)
8. [ ] Run `task verify`

## Validation

- [ ] All version strings produced by `Version::to_string()` match the OCI tag regex
- [ ] `Version::parse()` round-trips: `parse(v.to_string()) == Some(v)` for all constructible versions
- [ ] Cascade algebra tests pass unchanged
- [ ] Acceptance tests pass (push with build-tagged versions to registry:2)
- [ ] `task verify` passes

## Links

- [OCI Distribution Spec — Tag Grammar](https://github.com/opencontainers/distribution-spec/blob/main/spec.md)
- [distribution-spec#154 — Add `+` as valid tag character](https://github.com/opencontainers/distribution-spec/issues/154)
- [Helm `+` → `_` convention](https://github.com/helm/helm/issues/10250)
- [ADR: Cascade Platform-Aware Push](./adr_cascade_platform_aware_push.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-12 | mherwig | Initial draft |
