# ADR: Platform-Aware Cascade: Per-Platform Version Filtering and Index Merging

## Metadata

**Status:** Proposed
**Date:** 2026-03-11
**Deciders:** mherwig
**Beads Issue:** TBD
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** oci, publishing, cascade
**Supersedes:** N/A
**Superseded By:** N/A

---

## Context

`ocx package push --cascade` automates the OCI "rolling tag" pattern: pushing
`cmake:3.28.1+build.20260311` for `linux/amd64` should also update `cmake:3.28.1`,
`cmake:3.28`, `cmake:3`, and `cmake:latest` if no newer version exists at each level.

Two independent bugs exist in the current implementation. Both stem from the same
root assumption: that tags in the registry are platform-agnostic. They are not.

---

### Bug 1 — Platform-blind version comparison

**Location:** `crates/ocx_cli/src/command/package_push.rs` lines 55–76

```rust
let other_tags = context.remote_client()?.list_tags(identifier.clone()).await?;
let other_versions = other_tags
    .into_iter()
    .filter_map(|tag| Version::parse(&tag))
    .collect::<Vec<_>>();
let (cascaded_versions, is_latest) = version.cascade(other_versions);
```

The code collects **all** tags for the repository and feeds them to
`Version::cascade()` as "other versions that might be newer". But a tag like
`3.28.2` may only exist for `linux/amd64`. If the current push is for
`darwin/arm64`, `3.28.2` is irrelevant — it does not block the cascade for
`darwin/arm64`.

Current behaviour: the cascade algorithm sees `3.28.2 > 3.28.1+build.1` and
stops propagating at the patch level, so `cmake:3.28` is **not updated** for
`darwin/arm64`, even though `3.28.1+build.1` is the newest version available on
that platform.

The bug is actually documented at the library level
(`crates/ocx_lib/src/package/version.rs` lines 100–103):

```
/// Attention: This function is not platform aware.
/// The caller is responsible for ensuring that the versions in `others`
/// are all from the same platform as `self`.
```

The CLI command never fulfils this contract.

---

### Bug 2 — Copy-manifest overwrites multi-platform indexes

**Location:** `crates/ocx_cli/src/command/package_push.rs` lines 100–112

```rust
let (digest, manifest) = context
    .remote_client()?
    .push_package(package_info.clone(), self.content.clone())
    .await?;
let source_identifier = identifier.clone_with_digest(digest);
for identifier in versions_to_push {
    context
        .remote_client()?
        .copy_manifest_data(&manifest, &source_identifier, identifier.tag_or_latest())
        .await?;
}
```

`push_package` returns an `oci::Manifest::ImageIndex` that contains **only the
platform just pushed**. The outer loop then uses `copy_manifest_data` to push
this single-platform index verbatim to every cascaded tag (`3.28.1`, `3.28`,
`3`, `latest`).

Any existing entries for other platforms in those cascaded tags are **silently
destroyed**.

Example: `cmake:3.28` currently holds entries for `linux/amd64` and
`darwin/arm64`. A new `linux/amd64`-only push via `--cascade` overwrites
`cmake:3.28` with a single-platform index. `darwin/arm64` users who try
`ocx install cmake:3.28` after this push get a "platform not found" error.

**Contrast with the primary-tag push:** `update_image_index` (client.rs lines
397–464) correctly fetches the existing index, removes the old entry for the
target platform, and inserts the new one. The cascade path never calls this
merge logic — it uses the cheaper `copy_manifest_data` shortcut instead.

---

## Decision Drivers

1. **Correctness above all.** Users relying on rolling tags (`cmake:3.28`,
   `cmake:latest`) for installation in CI pipelines must not see intermittent
   "platform not found" failures caused by a single-platform push.

2. **Backward compatibility of the tag structure.** Rolling tags have always
   been multi-platform; publishers reasonably expect them to stay that way after
   a platform-specific push.

3. **Minimal extra registry round-trips.** The fix for Bug 1 requires fetching
   manifests for "blocker" versions. This must not degrade performance
   significantly for the common case of a fresh package with few tags.

4. **Atomic-enough semantics.** OCI registries do not support transactions. The
   push sequence must be ordered to minimise the window where a tag is in a
   degraded state.

---

## Considered Options

### Option A — Fetch all manifests, then filter (for Bug 1)

Fetch the manifest for every existing tag, inspect whether the platform is
present, and pass only matching tags to `version.cascade()`.

**Pro:** Always correct; no partial solution.
**Con:** O(n) manifest fetches where n = total tag count. For large public
packages (cmake, llvm) with hundreds of tags this is prohibitively slow.

---

### Option B — Fetch manifests only for "blocking" tags (for Bug 1) ✓ Chosen

A tag can only block cascade if it is _strictly greater_ than the pushed
version at the same version level. The set of blocking candidates is small
(typically 1–3 tags). Fetch only those manifests, check for platform
presence, and exclude any that do not support the pushed platform.

Algorithm:

1. Parse all existing tags into `Version` objects.
2. Run `version.cascade(other_versions)` once with **all** versions
   (platform-blind) to determine the candidate blocker set: the set of versions
   that caused the cascade to stop at any level.
3. Fetch manifests for only those blocker candidates.
4. Remove blockers that do not contain the pushed platform.
5. Re-run `version.cascade(filtered_versions)` with the corrected set.

This is at most a handful of extra HTTP requests — comparable to the existing
`list_tags` call already made.

**Pro:** Correct; bounded number of extra manifest fetches.
**Con:** Two-pass cascade computation; moderately more complex.

---

### Option C — Ignore platform in cascade, rely solely on merge (for Bug 1)

Do not fix Bug 1 independently. Accept that rolling-tag cascade decisions may
occasionally miss a tag for a specific platform, and rely on publishers to
notice and re-push.

**Con:** Platform drift is silently ignored. Tag databases grow asymmetrically
across platforms over time and become hard to reason about. Rejected.

---

### Option D — Merge index for cascaded tags (for Bug 2) ✓ Chosen

For every cascaded tag (not just the primary), apply the same merge logic
used in `update_image_index`:

1. Fetch (or create) the existing OCI image index for the cascaded tag.
2. Remove any entry for the pushed platform.
3. Add the new entry.
4. Push the updated index.

Introduce a new `Client` method — `merge_platform_into_index` — that
encapsulates this logic. Remove `copy_manifest_data` from the cascade path
(it remains useful for other callers, e.g., digest-addressed copies, so do
not delete it).

**Pro:** Correct; reuses existing `update_image_index` logic; clear API.
**Con:** N additional `pull_manifest` calls for cascaded tags (N ≤ depth of
version tree, typically 3–4). Each tag requires a read-before-write.

---

### Option E — Server-side copy with selective platform patch

Use an OCI distribution spec PATCH or referrers API to update only the
relevant platform entry in an existing index without a full read-modify-write.

**Con:** Not supported by `registry:2` or most hosted registries. Rejected.

---

## Decision Outcome

**Bug 1 fix:** Option B — two-pass cascade with platform-filtered blockers.
**Bug 2 fix:** Option D — per-cascaded-tag index merge via `merge_platform_into_index`.

Both changes are in the `package_push` command and the OCI `Client`. The
`version.rs` cascade logic itself requires **no changes** — the fix is entirely
in the caller.

---

## Implementation Sketch

### New `Client` method (Bug 2)

```rust
/// Fetches the image index at `target_tag`, removes any existing entry for
/// `platform`, inserts `manifest_entry`, and pushes the updated index.
///
/// Equivalent to `update_image_index` but operates on an arbitrary tag
/// without the package blob upload that precedes it during `push_package`.
pub async fn merge_platform_into_index(
    &self,
    source_identifier: &Identifier,
    target_tag: impl Into<String>,
    platform: &oci::Platform,
    manifest_sha256: &str,
    manifest_size: i64,
) -> Result<()>;
```

The method body mirrors `update_image_index` lines 407–463 almost verbatim,
parameterised on `target_tag` and `platform` instead of reading them from
`package_info`.

### Updated cascade push loop (Bugs 1 + 2)

```rust
// 1. Collect all existing tags.
let other_tags = remote_client.list_tags(identifier.clone()).await?;
let other_versions: Vec<Version> = other_tags.iter()
    .filter_map(|t| Version::parse(t))
    .collect();

// 2. First-pass cascade (platform-blind) to identify blockers.
let (_, _) = version.cascade(other_versions.clone()); // result discarded
// Determine which tags caused the cascade to stop (i.e. are "blocking").
let blocker_tags = compute_blockers(&version, &other_tags, &other_versions);

// 3. Fetch manifests for blocker tags only; drop those lacking our platform.
let platform_versions: Vec<Version> = {
    let mut filtered = other_versions.clone();
    for blocker_tag in &blocker_tags {
        let tag_id = identifier.clone_with_tag(blocker_tag.clone());
        if let Ok((_, manifest)) = remote_client.fetch_manifest(&tag_id).await {
            if !manifest_has_platform(&manifest, &self.platform) {
                filtered.retain(|v| v.to_string() != *blocker_tag);
            }
        }
    }
    filtered
};

// 4. Second-pass cascade with platform-filtered versions.
let (cascaded_versions, is_latest) = version.cascade(platform_versions);

// 5. Push primary tag (merge path via update_image_index).
let (_, image_manifest) = remote_client
    .push_package(package_info.clone(), self.content.clone())
    .await?;
let manifest_sha256 = extract_sha256(&image_manifest);
let manifest_size  = extract_size(&image_manifest);

// 6. Cascade: merge into each parent tag's index.
for cascaded_id in cascaded_identifiers {
    remote_client
        .merge_platform_into_index(
            &identifier,
            cascaded_id.tag_or_latest(),
            &self.platform,
            &manifest_sha256,
            manifest_size,
        )
        .await?;
}
```

`compute_blockers` is a pure function that, given the pushed version and all
existing versions, returns the subset of existing version strings that would
suppress cascade at some level. It mirrors the break conditions in
`Version::cascade` but collects the stopping versions instead of discarding them.

---

## Push Order and Partial-Failure Safety

The recommended push order for a build-tagged version cascading to patch, minor,
major, and latest:

```
1. push build-tagged version (primary, most specific)
2. merge into patch  (e.g. 3.28.1)
3. merge into minor  (e.g. 3.28)
4. merge into major  (e.g. 3)
5. merge into latest
```

Most-specific tags are updated first. If the process is interrupted after step 1,
rolling tags remain at the previous version — degraded but not broken. If
interrupted after step 3, `cmake:3.28` is correct and only `cmake:3` / `latest`
lag. This is the minimum-impact failure ordering.

A future improvement could retry failed merge steps, but that is out of scope
for this ADR.

---

## Consequences

### Positive

- Rolling tags reliably reflect the newest version **per platform**, not the
  newest version across all platforms.
- A single-platform push no longer silently destroys platform entries in parent
  (rolling) tags.
- The cascade algorithm's documented contract (caller must filter by platform)
  is finally satisfied.
- `merge_platform_into_index` is a reusable primitive that can be used
  independently in future commands (e.g., `ocx package retag`).

### Negative / Trade-offs

- **2–6 extra manifest fetches per cascade push.** For the common case (< 10
  blockers, < 5 cascade levels) this adds ≈ 100–500 ms on a typical registry.
  Acceptable given that push is a human- or CI-triggered operation, not a hot
  path.
- **Read-before-write on each cascaded tag.** This is inherently racy on
  distributed registries. Two concurrent pushes to the same cascaded tag could
  overwrite each other's platform entry. This is the same race that exists in
  `update_image_index` today and is accepted as an operational constraint (do
  not push two different platforms simultaneously).
- **`--new` flag becomes slightly less useful.** Currently `--new` skips
  `list_tags`. With the two-pass approach, `--new` still skips `list_tags` and
  blocker manifest fetches, but the cascade merge reads each cascaded tag's
  manifest (which will 404 / empty for truly new packages). The empty-index
  fallback in `update_image_index` already handles this correctly.

---

## Alternatives Not Considered

- **Storing platform metadata in tag annotations.** OCI supports per-tag
  annotations. We could write the platform list into the index annotations and
  avoid manifest fetches for blockers. This requires registry support for
  annotation reads via a lightweight API — not universally supported.
- **Publisher-side platform manifest.** A dedicated registry tag (e.g.
  `__platforms__`) could store a JSON mapping of tag → platform list. Adds
  complexity and a non-standard convention.

---

## References

- `crates/ocx_cli/src/command/package_push.rs` — current cascade implementation
- `crates/ocx_lib/src/package/version.rs` lines 99–174 — `Version::cascade` and its platform contract note
- `crates/ocx_lib/src/oci/client.rs` lines 103–121 (`copy_manifest_data`), 397–464 (`update_image_index`)
- [OCI Image Index specification](https://github.com/opencontainers/image-spec/blob/main/image-index.md)
