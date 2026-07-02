# Research: Layer extraction / per-layer layout config

**Date:** 2026-07-02
**Feeds:** `adr_layer_layout_config.md` (Accepted), `plan_layer_layout_config.md`
**Method:** 3-axis parallel `worker-researcher` (annotation conventions · relocation patterns · prefix path-safety) + 1 gap-fill explorer. Findings synthesized by orchestrator.
**Bottom line:** all three axes independently corroborate the accepted ADR. No decision reversed. Research adds concrete engineering + security requirements now folded into the plan.

---

## Axis A — OCI annotation conventions (per-layer config home)

**Locked:** discrete flat keys on each layer descriptor's `annotations`, namespace `sh.ocx.layer.*`:

```
sh.ocx.layer.strip-components = "1"     # u8 as decimal string
sh.ocx.layer.prefix           = "bin"   # relative path, no leading/trailing slash needed
```

- `sh.ocx.layer.*` = textbook reverse-DNS (own `ocx.sh`), matches ecosystem (`land.oras.*`, `io.buildpacks.*`, `dev.cosignproject.*`). `org.opencontainers.*` is **reserved — MUST NOT** use.
- **Discrete keys, not a JSON blob.** Two scalars → flat keys are `jq`/`oras manifest fetch`-greppable, diff cleanly, evolve independently. JSON blob only justified for nested/variable shapes (buildpacks counter-example). Matches how `org.opencontainers.image.*` itself is designed.
- **Digest boundary (confirms ADR reuse safety):** annotations live on the *descriptor inside the manifest JSON*, not on the blob. Changing strip/prefix rewrites only the manifest (small doc); the **layer blob digest is untouched** → cross-package dedup preserved. Manifest digest changes (expected). This is exactly what lets two packages reuse one blob with different layout.
- **Registry support = non-issue.** Descriptor annotations are image-spec v1.0; registries treat manifest JSON as opaque. Observed "gaps" (Harbor #12593, GHCR) are UI-render only — irrelevant since `ocx` reads manifest JSON directly.
- **Caveat:** per-layer annotation *authoring* is thin in third-party CLIs (ORAS needed `--annotation-file` workaround 2023). Non-issue for OCX — we construct manifests programmatically via `oci-client`. Don't assume `oras push`/`docker buildx` ergonomics if a manual-push path is ever exposed.
- **Trend note:** OCI v1.1 (2024) pushes cross-manifest metadata toward `subject`/Referrers API (signatures, SBOMs). Does **not** apply here — strip/prefix are intrinsic 1:1 with a layer, so descriptor annotations are correct, not a referrer.

Sources: OCI image-spec `annotations.md` / `descriptor.md` / `manifest.md`; ORAS artifacts-spec#89 (`land.oras.*` precedent); Docker annotations docs; Helm #12159; Harbor #12593; OCI v1.1 blog.

## Axis B — strip/relocation patterns + CAS invariant

**Locked:** verbatim-at-ingest, transform-at-materialize (= ADR Part 1). Precedent: OCI (blob keyed by compressed-tar digest, rewriting at unpack), Nix (immutable store paths, relocation in derivations/profiles), Bazel (CAS keyed by raw bytes, `pkg_files` transform in action graph). OCX violated this by stripping into `layers/{digest}/content/`.

**Closest analog = Bazel `rules_pkg` `pkg_files`:** per-source pipeline `exclude → (renames XOR strip_prefix) → prefix`, feeding one `pkg_tar`. Same shape as OCX per-layer strip+prefix into one package tree. Posture: **"fail uniformly, don't partially apply"** + **error on destination collision at analysis time**.

**Collision policy — locked to error, not last-wins.** OCX multi-layer = flat spatial merge of independently-published layers, NOT a Docker/OverlayFS temporal override stack (no "later layer supersedes" semantic). Silent last-wins would drop content in an immutable store. Keep `AssemblyError::LayerOverlap`; run per-layer transform **before** overlap detection so post-transform collisions surface.

**`tar --strip-components` semantics a reimpl must honor (GNU tar):**
- Entry with < N components → strips to empty → **skip** (GNU warns; OCX currently skips *silently* at `tar.rs:186`).
- Directory header stripped to empty → skip, but children with enough depth still extract → parent dirs must be synthesized (`create_dir_all`). OCX already does this (`last_parent` dedup) — preserve when logic moves to assemble.
- GNU default flags `rsh` rewrite symlink+hardlink **targets** on strip. **OCX assemble does NOT rewrite targets** — it recreates symlinks verbatim. Correct *only* because strip removes N leading components uniformly and prefix prepends uniformly → relative links between surviving entries keep the same relative offset. Breaks if one endpoint strips away (dangling). → **regression test required.**
- Determinism: OCX tar uses `HeaderMode::Deterministic`; assemble transform must iterate manifest-ordered `layers[]`, never HashMap order.

**New requirements folded into plan:**
1. `debug!` log count of entries dropped-to-empty at assemble (whole-layer→zero = likely misconfig; currently no diagnostic trail).
2. Before deleting extraction-time strip, test how the `tar` crate resolves `EntryType::Link` (hardlink) targets — confirm independent of strip (Part 1 makes extraction always strip=0, which should *simplify* this, but verify).
3. Symlink dangling-after-strip regression test (target stripped away).

Sources: GNU tar manual (transform / hard-links / symlinks); OCI `layer.md` (whiteout); kernel overlayfs.txt; Bazel `rules_pkg`; NixOS wiki + Nix profiles.

## Axis C — `prefix` path-traversal safety (trust boundary)

**Locked recommendation — no new dependency; extend existing helpers.**

New primitive in `utility/fs/path.rs`:
```
join_under_root(root, untrusted_relative) -> Result<PathBuf, PathEscapeError>
  1. reject is_absolute()
  2. reject Windows drive-letter (^[A-Za-z]:) / UNC (^\\ or //) / verbatim (\\?\)  <-- host-independent
  3. lexical_normalize (already backslash-aware)
  4. reject residual `..` (escapes_root)
  5. join, re-verify starts_with(normalized root)   <-- belt & suspenders
```
Refactor `symlink::validate_target` (`symlink.rs:36-39`) — inline duplicate of join+starts_with — to call `join_under_root` (legit 2nd-caller DRY per quality-core).

**The critical, OCX-specific bypass:** `std::path::Path::is_absolute()` parses Windows drive/UNC prefixes **only on a Windows target**. Publish CI (Linux) validating a `prefix` of `C:\Windows` with an `is_absolute()`-only check **passes it**, then the same string joined on a Windows read host fully escapes (`PathBuf::push` replaces the base on absolute/prefixed push). Step 2's explicit host-independent string check is mandatory — neither `lexical_normalize` nor `escapes_root` does this today.

**Read-time physical guard (lexical analysis cannot cover this):** wire in `utility::fs::refuse_if_symlink_in_path` (`symlink_walk.rs`) — **fully implemented + unit-tested, currently ZERO callers** — against the prefix-resolved destination before the first `create_dir`/hardlink under it. Closes the "symlinked intermediate directory" class: RUSTSEC-2021-0080, CVE-2021-32803 (node-tar dir-cache poisoning), CVE-2026-33056 (`tar-rs` chmod-through-symlink via `metadata` vs `symlink_metadata`, fixed 0.4.45).

**Validate at BOTH boundaries** (mirrors existing symlink two-boundary pattern, `assemble.rs:556-560`):
- **Publish** (`LayerRef::FromStr` / push) — fail fast, full cross-platform check; well-poisoning prevention.
- **Read** (assemble) — re-run full lexical check on every annotation (registries are third-party-writable: `[mirrors]`, cascade, compromised registry) **+** `refuse_if_symlink_in_path`. Content-addressed store must not trust ingest.

**Why lexical, not canonicalize:** `prefix` is validated before the package tree exists (publish) and is meant to *create* new dirs — `canonicalize` requires existence and follows symlinks. OCX module doc already states this (`path.rs:6-12`).

**Dep hygiene:** `Cargo.lock` pins `tar = 0.4.46`, `zip = 8.6.0` — both past their CVE fixes (tar ≥0.4.45, zip ≥2.3.0). New `prefix`-join code is OCX's own logic, not covered by those fixes.

**Follow-up (out of scope, note only):** `archive/zip.rs` relies on `enclosed_name()` and does NOT call `escapes_root`, asymmetric with `archive/tar.rs`. Flatten onto `join_under_root` later.

Sources: CWE-22/23; Snyk Zip Slip; GHSA-94vh-gphv-8pm8 (CVE-2025-29787 rust `zip`); RUSTSEC-2021-0080; CVE-2026-33056 + Rust blog; CVE-2021-32803/32804 (node-tar); cap-std; `PathBuf::push`/`std::path::Prefix` docs.

---

## Decisions locked (feed the plan)

| # | Decision | Source axis |
|---|---|---|
| 1 | Per-layer config = discrete `sh.ocx.layer.{strip-components,prefix}` descriptor annotations | A |
| 2 | Emit annotation only when explicitly set; default publish = `annotations: None` (byte-identical manifests) | A + owner |
| 3 | Resolve chain per layer: annotation → `Bundle.strip_components` → 0 | A + owner |
| 4 | Extract verbatim (strip=0) into layer store; transform at assemble | B |
| 5 | Thread parallel `&[LayerLayout]` (manifest order) into `assemble_from_layers`; transform per file before overlap check | B + gap-fill |
| 6 | Collision = hard error (`AssemblyError::LayerOverlap`), never last-wins | B |
| 7 | `debug!` dropped-to-empty count; regression-test symlink/hardlink dangling-after-strip | B |
| 8 | New `join_under_root` (lexical + host-independent Win drive/UNC reject + post-join check); refactor `symlink::validate_target` onto it | C |
| 9 | Wire `refuse_if_symlink_in_path` at read-time before first write under prefix | C |
| 10 | Validate prefix at publish (`LayerRef::FromStr`) AND read (assemble) | C |
| 11 | No new dependency; lexical not canonicalize | C |
| 12 | Part 1 touches only OCI layer-pull path; leave `Archive::extract_with_options` (BundleBuilder + archive tests) unchanged | gap-fill |
