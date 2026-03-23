# Plan: Variant ADR Phase 4 + Documentation

## Context

Phases 1-3 of the variants ADR are complete: variant-aware `Version` parsing, cascade isolation, and mirror pipeline support. Phase 4 is the final implementation phase — annotations on OCI manifests, a `--variants` discovery flag on `index list`, and acceptance tests. Separately, the user guide needs a Variants section under Tags to document the `<variant>-<version>` convention.

## Part A: Annotations on Push

### Design: Auto-populate from tag + optional extra annotations

`push_image_manifest()` will auto-parse the variant from the tag via `Version::parse()` and set `sh.ocx.variant` on the manifest. An `annotations: Option<HashMap<String, String>>` parameter allows callers (mirror pipeline) to also pass `sh.ocx.variant.default`. This minimizes API churn — most callers pass `None` and get variant annotations automatically.

### Steps

**A1. Add annotation constants** — `crates/ocx_lib/src/oci/annotations.rs`
- Add `VARIANT = "sh.ocx.variant"` and `VARIANT_DEFAULT = "sh.ocx.variant.default"`

**A2. Add annotations to `push_image_manifest()`** — `crates/ocx_lib/src/oci/client.rs`
- Add `annotations: Option<HashMap<String, String>>` parameter
- Inside the method: parse variant from `package_info.identifier.tag_or_latest()`, build a base annotation map with `sh.ocx.variant` if variant detected, merge caller-provided annotations, set `manifest.annotations`
- Update `push_package()` in same file to accept and forward `annotations` parameter

**A3. Thread through Publisher** — `crates/ocx_lib/src/publisher.rs`
- Add `annotations: Option<HashMap<String, String>>` to `Publisher::push()` and `Publisher::push_cascade()`
- Forward to `client.push_package()` and `cascade::push_with_cascade()`

**A4. Thread through cascade** — `crates/ocx_lib/src/package/cascade.rs`
- Add `annotations` parameter to `push_with_cascade()`, forward to `client.push_image_manifest()`

**A5. Update CLI `package push`** — `crates/ocx_cli/src/command/package_push.rs`
- Pass `None` for annotations in `publisher.push()` / `publisher.push_cascade()` calls
- Variant annotation is auto-populated from tag in `push_image_manifest()`

**A6. Wire mirror variant annotations** — `crates/ocx_mirror/src/pipeline/push.rs`
- Build annotations from `VariantContext`: `{ sh.ocx.variant: name, sh.ocx.variant.default: is_default }`
- Pass to `publisher.push_cascade()` / `publisher.push()`
- For default alias cascade (second push with `without_variant()` tag), pass same annotations (those unadorned tags ARE the default variant)

**A7. Clean up mirror `annotations.rs`** — `crates/ocx_mirror/src/annotations.rs`
- Remove `#[allow(dead_code)]` from `build_annotations()`
- Add `variant_annotations(name: &str, is_default: bool) -> HashMap<String, String>` helper

## Part B: `--variants` Discovery Flag

**B1. Add flag to `index list`** — `crates/ocx_cli/src/command/index_list.rs`
- Add `#[arg(long, conflicts_with = "with_platforms")] variants: bool`
- When set: parse all tags via `Version::parse()`, collect unique `variant()` values
- Report via new `Tags::with_variants()` constructor

**B2. Extend `TagsData`** — `crates/ocx_cli/src/api/data/tag.rs`
- Add `WithVariants(HashMap<String, Vec<String>>)` variant (package → sorted variant names)
- `Printable` renders as two-column table: Package | Variant
- Tags without variant prefix are omitted (they represent the default variant which has no explicit name in tag space)

## Part C: Acceptance Tests

New file: `test/tests/test_variants.py`

| Test | What it verifies |
|------|-----------------|
| `test_install_variant_package` | Push `debug-1.0.0` with cascade, install `repo:debug-1.0.0`, verify candidate symlink |
| `test_install_variant_rolling_tag` | Push `debug-1.2.3` with cascade, install `repo:debug-1`, verify resolution |
| `test_select_variant_package` | Install variant, select, verify `current` symlink |
| `test_index_list_variants` | Push multiple variant tags, `ocx index list --variants`, verify output |
| `test_variant_and_default_coexist` | Push `debug-1.0.0` and `1.0.0`, install both, verify independent candidates |

Annotation tests (checking manifest annotations from registry) are optional — they require pulling the platform-specific manifest from the registry, which adds test infrastructure complexity. The annotations are a metadata concern primarily useful for tooling introspection, not for user-facing behavior.

## Part D: User Guide Documentation

File: `website/src/docs/user-guide.md`

**D1. Add `### Variants {#versioning-variants}` section** — insert between Tags (`### Tags`) and Cascades (`### Cascades`), around line 256.

Structure (following idea→problem→solution pattern from `documentation.md`):

1. **The idea** — A binary tool can be built multiple ways: optimization profiles, feature toggles, size trade-offs. These are variants — orthogonal to platform.
2. **The problem** — Docker's suffix convention (`3.12-slim`) creates parsing ambiguity with semver prereleases (`3.12-alpha`). Without variant support, publishers duplicate packages (`python-pgo`, `python-debug`).
3. **The solution** — ocx uses variant-prefix format: `<variant>-<version>`. Unambiguous because variants start with `[a-z]`, versions start with `[0-9]`.

Content:
- Tag format table (variant-version examples for python)
- Default variant = unadorned tags explanation
- Cascade per variant track — brief paragraph with cross-ref to Cascades section
- `:::tip` with `ocx index list python --variants` for discovery
- `:::warning` distinguishing OCX software variants from OCI `platform.variant` (CPU sub-arch like ARM v7/v8)

**D2. Update Platforms details box** — around line 316
- Add a sentence in the "Richer than OS and architecture" details box noting that OCI `platform.variant` is CPU sub-architecture, distinct from OCX [software variants][versioning-variants].

**D3. Update Cascades section** — around line 257
- Add a brief note that cascades operate per-variant track: `debug-3.12.5` cascades to `debug-3.12` → `debug-3` → `debug`, never crossing into other variant tracks.

**D4. Add link references** at bottom of file for new anchors.

## Verification

1. `cargo check --workspace` after each annotation threading step
2. `cargo nextest run --workspace` — existing unit tests pass
3. `task test` — new and existing acceptance tests pass
4. `task verify` — full quality gate
5. `task website:serve` — verify new Variants section renders correctly

## File Summary

| File | Change |
|------|--------|
| `crates/ocx_lib/src/oci/annotations.rs` | Add VARIANT, VARIANT_DEFAULT constants |
| `crates/ocx_lib/src/oci/client.rs` | Add annotations param to push_image_manifest, push_package |
| `crates/ocx_lib/src/publisher.rs` | Thread annotations through push, push_cascade |
| `crates/ocx_lib/src/package/cascade.rs` | Thread annotations through push_with_cascade |
| `crates/ocx_cli/src/command/package_push.rs` | Pass None for annotations |
| `crates/ocx_cli/src/command/index_list.rs` | Add --variants flag |
| `crates/ocx_cli/src/api/data/tag.rs` | Add WithVariants enum arm |
| `crates/ocx_mirror/src/pipeline/push.rs` | Build and pass variant annotations |
| `crates/ocx_mirror/src/annotations.rs` | Add variant_annotations helper, remove dead_code |
| `test/tests/test_variants.py` | New acceptance test file |
| `website/src/docs/user-guide.md` | Add Variants section, update Platforms + Cascades |
