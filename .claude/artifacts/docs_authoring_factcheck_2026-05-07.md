# Authoring Docs Fact-Check — 2026-05-07

Aggregated findings from 35 parallel Sonnet fact-check agents covering every H2/H3 subsection in the eight new `website/src/docs/authoring/*.md` pages.

Files in order:
- bp__byo-archives.md
- bp__cascade.md
- bp__first-push.md
- bp__layer-reuse.md
- bundle__compression.md
- bundle__sidecars.md
- bundle__stable.md
- bundle__strip-components.md
- bundle__what-goes-in.md
- deps__edge-visibility.md
- deps__name-field.md
- deps__ordering.md
- deps__pinning.md
- deps__when.md
- env__last-wins.md
- env__migrating.md
- env__templates.md
- env__types.md
- env__visibility.md
- ep__naming.md
- ep__python-example.md
- ep__target.md
- ep__when.md
- ep__why.md
- index__decisions.md
- index__journey.md
- index__tldr.md
- mig__describe.md
- mig__github-releases.md
- mig__homebrew.md
- mig__mirror.md
- mp__concept.md
- mp__metadata.md
- mp__pattern.md
- mp__stability.md

---

## website/src/docs/authoring/building-pushing.md#byo-archives — Bring Your Own Archives

### Verified

- **`.tar.gz` / `.tar.xz` as accepted file-layer formats.** `media_type_from_path()` in `crates/ocx_lib/src/media_type.rs:34–43` accepts exactly `.tar.gz`, `.tgz`, `.tar.xz`, `.txz` for `LayerRef::File` paths, returning the appropriate OCI media type. The doc's prose listing `.tar.gz` / `.tar.xz` is technically accurate as the canonical pair.

- **`.tgz` / `.txz` aliases are accepted** for both file layers (via `media_type_from_filename`, `media_type.rs:36–39`) and digest layer references (via `ArchiveMediaType::extensions()`, `crates/ocx_lib/src/publisher/layer_ref.rs:46–50`). The doc text omits these aliases, which is a drift concern addressed under Missing nuance below.

- **Zero file layers is valid.** `crates/ocx_cli/src/command/package_push.rs:51–52` explicitly documents "Zero layers is valid (produces a config-only OCI artifact) when `--metadata` is supplied." The `push_multi_layer_manifest` in `crates/ocx_lib/src/oci/client.rs:521–624` constructs `layer_descriptors: Vec<oci::Descriptor>` via `stream::iter(layers.iter())` — an empty slice yields an empty `Vec`, so `layers: []` in the resulting manifest is structurally valid without any guard or error path.

- **`--metadata` mandatory when no file layers.** `crates/ocx_cli/src/command/package_push.rs:65–76`: when `self.metadata` is `None`, the code looks for the first `LayerRef::File` element; if none is found (i.e., all layers are digest refs, or the layer list is empty) it returns `Err(anyhow!("--metadata is required when no file layers are provided"))`. This matches the doc's claim.

- **`[cmd-package-push]` link target.** `website/src/docs/authoring/building-pushing.md:74` resolves to `../reference/command-line.md#package-push`. The anchor `{#package-push}` exists at `website/src/docs/reference/command-line.md:830`. Valid.

- **`[cmd-package-create]` link target.** `building-pushing.md:73` resolves to `../reference/command-line.md#package-create`. The anchor `{#package-create}` exists at `command-line.md:773`. Valid.

- **`[authoring-bundle-anatomy]` link target.** `building-pushing.md:81` resolves to `./bundle-anatomy.md`. `website/src/docs/authoring/bundle-anatomy.md` exists. The link text refers to "Bundle Anatomy" without a subsection anchor — the file exists and has a root `# Bundle Anatomy`-level heading. Valid.

- **Digest non-determinism rationale.** The stated reason — "Re-bundling the same content yields a non-deterministic digest (timestamps, compression entropy)" — is consistent with `bundle-anatomy.md#stable` content and with the reference page warning at `command-line.md:872–874`. No contradiction found.

### Inconsistent / hallucinated [Block]

- **"referrer-only manifests (description metadata, signature attestations)."** No referrer API support, no subject-manifest field, and no signature attestation logic exists anywhere in `crates/ocx_lib/src/`. A search for `referrer`, `referrers`, `subject`, `attestation`, and `push_referrer` across the entire library returns zero results in production code. The zero-layer path is used for OCX's own description artifact (`push_description`, `publisher.rs:79`) which is pushed to a fixed `__ocx.desc` tag — this is not an OCI referrers-API construct and has no `subject` field. Calling the config-only artifact "useful for referrer-only manifests" is an unimplemented use case presented as current capability. The description-artifact and signature-attestation use cases are aspirational (see `adr_oci_artifact_enrichment.md`), not shipped. `command-line.md:832` partially corrects this ("referrer-only / description-only manifest") but still implies referrer-API usage. The body text in `building-pushing.md:25` is the stronger overclaim.

### Missing nuance / drift [Warn]

- **"Every file layer must be a pre-built `.tar.gz` / `.tar.xz` archive"** — omits `.tgz` and `.txz`. `media_type_from_filename()` (`media_type.rs:36–39`) and `ArchiveMediaType::extensions()` (`layer_ref.rs:46–50`) both accept `.tgz` and `.txz` as fully supported aliases for file layers and digest layer references respectively. The reference page (`command-line.md:844`) correctly lists all four forms (`.tar.gz`, `.tgz`, `.tar.xz`, `.txz`). The BYO-archives prose creates a false impression that only the two canonical forms are valid.

- **`--metadata` mandatory condition is broader than stated.** The doc says "`--metadata` becomes mandatory in that case" where "that case" means zero file layers. The actual condition in `package_push.rs:68–73` is "no file layers" — meaning any combination with no `LayerRef::File` elements (e.g., all-digest layers with no empty total) also triggers the requirement. Zero total layers is one subcase; all-digest-refs is another. The doc's wording ("no file layers") happens to match the actual error text in `package_push.rs:73` and is therefore technically correct for the zero-layer case, but the prose implies `--metadata` is only needed when the layer list is entirely empty, not also when every layer is a digest reference. The `[cmd-package-push]` reference page (`command-line.md:855`) is more precise: "Required when no file layers are provided (all layers are digest references, or the layer list is empty)."

- **"no sidecar to sniff"** — slightly imprecise. The sidecar inference looks for the first `LayerRef::File` path and derives `<basename>-metadata.json` from it (via `conventions::infer_metadata_file`). "Sniff" implies content inspection; it is purely a filename derivation. Low-severity prose imprecision.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none) — no code examples appear in the `#byo-archives` subsection itself.

### Style / convention violations [Warn]

- **Missing `.tgz`/`.txz` tooltip.** `docs-style.md` recommends `<Tooltip>` for jargon or technical aliases. The four-form equivalence (`.tar.gz` = `.tgz`, `.tar.xz` = `.txz`) is the kind of nuance that fits a tooltip rather than forcing a digression in the main paragraph.

- **`--metadata` mandatory note is prose-only.** `docs-style.md` specifies `:::warning` for "important caveats, commonly misunderstood things." The constraint that `--metadata` becomes required when no file layers are provided is exactly that kind of gotcha — the reference page already uses `:::warning` for the "bring your own archives" constraint. Marginal Warn: not a blocker, but the callout pattern is already used nearby and consistency would help.

- **"signature attestations"** — borderline jargon with no tooltip. Per `docs-style.md`, technical terms that interrupt prose flow are good tooltip candidates. Given the larger Block finding that referrer/attestation is unimplemented, the whole phrase should be removed rather than tooltipped.

---

## website/src/docs/authoring/building-pushing.md#cascade — Cascading Rolling Tags

### Verified

- **`--cascade` flag exists and is correctly named.** `crates/ocx_cli/src/command/package_push.rs:18` declares `#[clap(long = "cascade", short = 'c')]`. `./target/release/ocx package push --help` confirms `-c, --cascade` with description "Will cascade rolling releases, ie. pushing 1.2.3 will also update 1.2, 1, etc".

- **"OCX consults the existing tags" on cascade.** `package_push.rs:95` calls `publisher.list_tags(identifier.clone()).await` before delegating to `publisher.push_cascade(...)`. Matches the prose claim that OCX "consults the existing tags".

- **Cascade re-points ancestor rolling tags.** `crates/ocx_lib/src/package/cascade.rs:122-136` (`cascade()` function) computes the version chain from most-specific to least-specific, yielding tags to re-point. `resolve_cascade_tags()` (line 149) and `push_with_cascade()` (line 193) orchestrate the actual OCI writes. Ancestor list (patch → minor → major → latest) matches the example in the doc.

- **"Only when the new tag is genuinely the latest at that specificity level."** Verified by blocker logic in `decompose()` (`cascade.rs:50-116`): for each level, `blockers` are versions in the range `(current, parent)` — i.e., versions newer than the candidate at that specificity. If any blocker exists, cascade halts at that level. Test `cascade_blocked_at_minor_level` (line 282) and related tests confirm correct cutoff behavior.

- **Backport example accuracy.** The doc says "Push a backport `0.9.5` after `1.0.1` is live and `--cascade` won't touch `latest`." The code's version ordering (`BTreeSet<Version>` with `Excluded` bounds) ensures that if `1.0.1` exists in `others`, it appears in `latest_blockers` when computing cascade for `0.9.5`, so `is_latest` returns `false`. Test `old_version_blocked_by_newer_at_minor` (line 499) directly validates this pattern. The doc example is simplified (uses `0.9.5` vs `1.0.1` rather than the code's `0.x` vs `1.x`) but the logic is accurate.

- **"Cascade is a publisher convention, not a registry-enforced rule."** Confirmed by `subsystem-package.md` ("Cascade = publisher convention (not registry-enforced)"), `arch-principles.md` ("Cascade = Publisher convention: push `3.28.1` and auto-update `3.28`, `3`, `latest` tags"), `in-depth/versioning.md:93` ("This is a publishing convention, not a guarantee enforced by the registry."), and `docs-style.md` Precision and Nuance section ("Cascade is convention, not enforced.").

- **"The registry sees only tag-to-digest writes."** Accurate at the OCI layer. Cascade logic is entirely in `cascade.rs` (client-side); the OCI registry receives only individual tag mutation calls via `client.push_manifest_and_merge_tags()`.

- **`[in-depth-versioning-cascades]` link target exists.** `website/src/docs/in-depth/versioning.md:75` declares `## Cascades {#cascades}`, which resolves the `../in-depth/versioning.md#cascades` link definition at `building-pushing.md:78`.

- **`<Terminal src="/casts/package-cascade.cast" />` file exists.** `website/src/public/casts/package-cascade.cast` is present (4.2K). Cast content shows commands `ocx package push -n -c -p linux/amd64 ... mytool:1.0.0 ...` then `ocx package push -c -p linux/amd64 ... mytool:1.0.1 ...`, followed by `ocx index update mytool` and `ocx index list mytool`. Output shows tags `1`, `1.0`, `1.0.0`, `1.0.1`, `latest` — consistent with cascade behavior described in the section.

- **Recording script `test/recordings/scripts/package-cascade.sh` aligns with prose.** Script pushes `mytool:1.0.0` with `-n -c` (new + cascade), then `mytool:1.0.1` with `-c` only, then runs `ocx index update` and `ocx index list`. This directly illustrates the "push `mytool:1.0.1`, OCX re-points `1.0`, `1`, `latest`" described in the section. No discrepancy found.

- **Prose → Terminal → style flow is clean.** Section has exactly two body paragraphs followed by a `<Terminal>` block. No heading inside the section. Consistent with doc style conventions.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **Cascade example uses `1.0.0` / `1.0.1` but ancestors cited are `1.0`, `1`, `latest`.** The actual cascade chain for `1.0.1` would produce ancestors `1.0.1` (patch rolling), `1.0` (minor rolling), `1` (major rolling), `latest`. The doc lists only the three rolling parents (`1.0`, `1`, `latest`) without mention of the patch-level rolling tag `1.0.1` itself. This is not wrong — publishers using the convention typically push `1.0.1` as the versioned tag and the doc is discussing the *alias* graph — but it omits the patch rolling tag from the ancestor enumeration. The cast output confirms `1.0`, `1.0.0`, `1.0.1`, `latest` are all set, so `1.0.0` and `1.0.1` are both present as patch-level rolling tags, not shown in the inline example. Minor omission; readers may assume the ancestor set is exhaustive. [Warn: imprecise enumeration of cascade levels]

- **"OCX synthesises the alias semantics on top."** Accurate but incomplete — the cascade also checks blocker platform membership (`has_blocking_platform()` in `cascade.rs:220`), meaning the cascade decision is platform-aware, not purely version-ordered. A newer version that exists for only a different platform does not block the cascade for the current platform. The prose does not surface this nuance, which is significant for multi-platform publishers. [Warn: missing multi-platform nuance in cascade description]

- **"Consumers reach the cascade via [pinning][in-depth-versioning-cascades] in the user guide."** The link target (`in-depth/versioning.md#cascades`) is the in-depth versioning page, not the user guide. The user guide (`user-guide.md`) references `in-depth/versioning.md` in a "Learn more" tip (line 68) but does not have a dedicated "pinning" section about cascade. Calling this "the user guide" is mildly misleading — the link resolves correctly to the in-depth page, but the prose frames it as user-guide content. [Warn: framing mismatch — link target is in-depth page, not user guide]

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none — cast file exists, content matches the section, script aligns with prose)

### Style / convention violations [Warn]

- **Duplicate link definition `[in-depth-versioning-cascades]` used for two semantically different display texts.** Line 31 uses `[pinning][in-depth-versioning-cascades]` (display text "pinning") and then immediately `[\`in-depth/versioning.md#cascades\`][in-depth-versioning-cascades]` (display text showing the raw path). Both resolve to `../in-depth/versioning.md#cascades`. The sentence reads "Consumers reach the cascade via [pinning][...] in the user guide and [`in-depth/versioning.md#cascades`][...]" — saying the same link twice with different framing. The second occurrence is redundant; it adds no navigation value over the first. [Warn: duplicate link target with different display text — second occurrence redundant, consider removing or replacing with a `See Also` entry only]

- **`alias graph` as potential tooltip candidate.** The phrase "alias graph" in "without maintaining the alias graph by hand" is a technical term (`docs-style.md` §"Tooltips" flags these as good candidates). It is used once without definition. Borderline — readers can infer the meaning from context, but a tooltip would improve discoverability. [Warn: borderline tooltip candidate]

---

## website/src/docs/authoring/building-pushing.md#first-push — The First Push

### Verified

- **`--platform` / `-p` required** — `#[clap(short, long, required = true)]` confirmed at
  `crates/ocx_cli/src/command/package_push.rs:33`. The reference doc at
  `website/src/docs/reference/command-line.md:852` ("required") agrees.

- **"uploads zero or more layers as OCI blobs"** — zero-layer support confirmed: field `layers: Vec<LayerRef>` (no `required` constraint), and the `LayerRef` doc comment reads "Zero layers is valid (produces a config-only OCI artifact)" (`publisher/layer_ref.rs:50-52`). `#byo-archives` section (lines 24-25 of the page) and the reference command-line doc (line 848) both confirm.

- **"records them under one image manifest in the order you give"** — the OCI client comment at `client.rs:522-524` reads "Upload file layers and verify digest layers concurrently, preserving input order so manifest descriptors match the caller-supplied order." `LayerRef` doc comment (`layer_ref.rs:96-99`) also calls out "index 0 is the base layer, index N is the top layer." Layer order is preserved in the manifest. **Verified.**

- **"OCI blobs … one image manifest"** — `push_multi_layer_manifest` (client.rs:512) uploads each layer as a blob and produces one `ImageManifest`. `merge_platform_into_index` (client.rs:183) then merges that single manifest into an `ImageIndex` under the tag. The text "one image manifest" is accurate (a single `ImageManifest` per platform-specific push). **Verified.**

- **Code example matches recording script** — `test/recordings/scripts/package-push.sh` contains exactly the two commands shown in the code fence:
  ```
  ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
  ocx package push -n -p linux/amd64 -m metadata.json mytool:1.0.0 mytool-1.0.0.tar.xz
  ```
  **Verified.**

- **`<Terminal src="/casts/package-push.cast" />` — cast file exists** — `website/src/public/casts/package-push.cast` confirmed present (2.3 KB).

- **`[cmd-package-push]` link anchor** — `#package-push` anchor exists at `website/src/docs/reference/command-line.md:830`. **Verified.**

- **`[authoring-multi-platform]` link** — `./multi-platform.md` confirmed present at `website/src/docs/authoring/multi-platform.md`. **Verified.**

- **`[oci-image-index]` URL** — `https://github.com/opencontainers/image-spec/blob/main/image-index.md` resolves to the OCI Image Index Specification page. **Verified.**

- **"single-platform push lands a single manifest under the tag"** — correct: `push_package` calls `push_manifest_and_merge_tags` which calls `merge_platform_into_index`, merging one manifest descriptor into the image index under the requested tag. Each platform push adds one entry. **Verified.**

---

### Inconsistent / hallucinated [Block]

- **`--new` purpose is misdescribed** — The doc says:
  > "pass `--new` to skip the pre-push tag listing — there are no prior tags to consult, and skipping the call shaves a round trip"

  The source code (`package_push.rs:94-113`) shows `--new` is **only active during a `--cascade` push**. When `self.cascade` is false (as in the example command, which uses neither `-c` nor `--cascade`), the code takes the `else` branch at line 112 and calls `publisher.push()` directly — **`--new` has no effect whatsoever on a plain (non-cascade) push.** Tag listing is only performed when `--cascade` is set; `--new` makes that tag-listing failure non-fatal (treats failure as "new package", substituting an empty vec). In a plain push there is no round-trip to skip — `--new` is inert.

  The reference doc at `command-line.md:854` correctly states "Skips the pre-push tag listing that is otherwise used for **cascade resolution**", making the cascade dependency explicit. The user-guide prose omits this crucial qualifier and implies `--new` is generally useful on first push of any package, which is incorrect.

  **Cite**: `crates/ocx_cli/src/command/package_push.rs:94-113`; `website/src/docs/reference/command-line.md:854`.

---

### Missing nuance / drift [Warn]

- **The example command uses `-n` without `-c`/`--cascade`, making `-n` a no-op** — As established above, `--new` is silently ignored when `--cascade` is not set. A reader who copies the command exactly will not observe any difference with or without `-n`. The flag is not harmful, but its inclusion in a "first push" example without `--cascade` is misleading and may train the reader to always include `-n` on first push, even though it only matters alongside `--cascade`. The example is not wrong syntactically but creates incorrect mental model.

- **"`--new` shaves a round trip"** — This claim requires qualification: the round trip saved is the `list_tags` call inside the cascade path (`publisher.list_tags`, client.rs:96). Without `--cascade`, no such call exists. The text states the savings unconditionally.

---

### Broken refs [Block]

- (none)

---

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **Cast title mismatch (Warn)** — The `<Terminal>` component specifies `title="Publishing a package for the first time"`, but the cast file header (`package-push.cast:1`) has `"title": "Publishing a package"` (no "for the first time"). The `title` prop on the component overrides the cast header in the player, so the rendered title will be "Publishing a package for the first time" — the component prop wins. Not a functional break, but the cast header is slightly stale relative to the component title.

- **Cast content matches example (OK)** — The decoded terminal output shows the exact two commands from the code fence (`ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz` then `ocx package push -n -p linux/amd64 -m metadata.json mytool:1.0.0 mytool-1.0.0.tar.xz`), plus two follow-up commands (`ocx index update mytool`, `ocx index list mytool`). The cast faithfully replays the documented workflow.

---

### Style / convention violations [Warn]

- **Code fence and Terminal cast cover the same commands (Warn)** — The code-fence block shows the two commands; the `<Terminal>` cast replays those same two commands (plus two follow-up index commands). The dual presentation is redundant for the first-push commands. Per `docs-style.md`, no explicit rule bans this combination, but `subsystem-website.md` shows the pattern as either cast-only or code-only for a given demo. Consider whether the cast alone (with `collapsed=false` or inline `<Frame>`) is sufficient, or whether the code-fence adds value by showing the flags more readably before the animation plays.

- **`collapsed` prop on `<Terminal>`** — The component has `collapsed`, meaning the animation is hidden behind a toggle on page load. This means a reader stepping through the prose sees the code-fence commands but must click to see the cast. For a "first push" introductory section the collapsed default may reduce impact — `subsystem-website.md` shows `collapsed` as optional; consider whether the default should be open for this high-value intro example. (Style suggestion, not a rule violation.)

---

## website/src/docs/authoring/building-pushing.md#layer-reuse — Reusing Layers Across Packages

### Verified

- **`sha256:<64-hex>.<ext>` form accepted.** `LayerRef::FromStr` in `crates/ocx_lib/src/publisher/layer_ref.rs:147–176` strips each known extension suffix, then calls `oci::Digest::try_from` on the remainder. For sha256 the remainder must be exactly 64 hex chars (`digest.rs:40`). All four listed aliases accepted: `tar.gz`, `tgz` → `ArchiveMediaType::TarGz`; `tar.xz`, `txz` → `ArchiveMediaType::TarXz` (`layer_ref.rs:46–50`). Tests at lines 190–234 confirm each alias parses correctly.

- **"OCI blob HEADs do not carry the original media type."** Confirmed at `client.rs:505–510` (doc comment) and `native_transport.rs:200–208` (implementation): `head_blob` calls `fetch_blob_size` which issues an HTTP HEAD and returns only `Content-Length`. The caller must supply the media type; that is why the extension suffix is mandatory.

- **"OCX HEADs the registry to verify the digest exists."** `client.rs:602`: `let size = self.transport.head_blob(&image, digest).await?;` is called for every `LayerRef::Digest` before recording the descriptor in the manifest. No upload path is taken. Verified by test stub at `client.rs:1844` asserting `head_blob` is called exactly once.

- **Hardlink behavior verified.** `assemble.rs:7,116,130,482–483`: `assemble_from_layers` hardlinks regular files from each layer's `content/` into the package's `content/`. `package_manager/tasks/pull.rs:350–360` calls this after layer extraction. The storage doc at `in-depth/storage.md:98,113` also documents this behavior explicitly.

- **`./` prefix forces file interpretation.** `layer_ref.rs:153`: `let looks_like_path = s.starts_with("./") || s.starts_with('/');` — both `./` and absolute paths bypass digest parsing. Test `parse_dot_slash_forces_file_even_on_digest_shape` at line 281 covers this explicitly.

- **Cast file exists.** `/website/src/public/casts/package-layer-reuse.cast` exists (232 lines, asciinema v2 format). The `Terminal` component at `building-pushing.md:53` uses `src="/casts/package-layer-reuse.cast"`.

- **`[in-depth-storage-layers]` resolves.** Link target `../in-depth/storage.md#layers` — `storage.md` exists and has `## Layers {#layers}` at line 96.

- **`[authoring-bundle-anatomy-stable]` resolves.** `bundle-anatomy.md` has `## Stable Archives {#stable}` at line 16.

- **`[in-depth-versioning-cascades]` resolves.** `versioning.md` exists and has `## Cascades {#cascades}` at line 75.

- **Numbered procedure is clear and well-structured.** Steps follow a logical push workflow (bundle → push file layer → push by digest). Style aligns with `docs-style.md`.

- **`:::warning` callout used appropriately.** The pathological-filename edge case is a genuine gotcha that justifies `:::warning` per `docs-style.md` callout table.

- **Three-tier storage model concept is real.** `arch-principles.md` and `adr_three_tier_cas_storage.md` describe the blobs / layers / packages three-tier CAS. `in-depth/storage.md` documents the same. The link `[three-tier storage model][in-depth-storage-layers]` correctly points to the layers section.

---

### Inconsistent / hallucinated [Block]

- **`sha256:<64-hex>.<ext>` is narrower than the parser accepts.** The doc presents the syntax as `sha256:<64-hex>.<ext>`, implying sha256 only. The parser (`layer_ref.rs:156–168`) calls `oci::Digest::try_from` which accepts `sha256` (64 hex), `sha384` (96 hex), and `sha512` (128 hex) — confirmed by test `parse_digest_sha512_with_ext` at `layer_ref.rs:237`. A publisher who supplies `sha512:<128-hex>.tar.xz` will find it works. The syntax block should read `<algorithm>:<hex>.<ext>` or at minimum note that sha256 is the common case; restricting to `<64-hex>` misstates the supported set. **Severity: Block** — a publisher with a sha384/sha512 registry digest who follows the docs literally would incorrectly conclude the form is unsupported.

- **"Bare `sha256:` tokens are always parsed as digest references" (in the warning callout) is inaccurate.** The code distinguishes three cases: (1) `sha256:<64hex>.<ext>` → `LayerRef::Digest` (the valid case), (2) `sha256:<64hex>` with no extension → `Err(LayerRefParseError::BareDigest)` rejected with an error message, (3) unrecognized strings → `LayerRef::File`. The callout text says bare `sha256:` tokens are "always parsed as digest references," which mischaracterizes behavior: bare digests are **rejected** with an error (`layer_ref.rs:170–171`), not parsed as digest references. The actual risk being warned against is `sha256:<hex>.<ext>` filenames being mistaken for digest references — and indeed those are parsed as `LayerRef::Digest`. The callout should say "tokens matching `sha256:<hex>.<ext>` are parsed as digest references; bare `sha256:<hex>` tokens (without extension) are rejected with an error." **Severity: Block** — the existing wording implies unexpected silent behavior where users actually get a clear error.

- **"The pattern … is visible in [`mirrors/cmake/`][mirror-cmake]."** The cmake mirror (`mirrors/cmake/mirror.yml`) does not use digest-based layer reuse. The ocx_mirror pipeline code (`crates/ocx_mirror/src/pipeline/push.rs:34`) only ever constructs `LayerRef::File`; the mirror tool has no mechanism to track a previously-pushed base layer digest and re-reference it. The claim that the mirrors/cmake configuration demonstrates this pattern is **incorrect** — the mirror tool always re-pushes all layers on each run. Publishers who look at that file will find no digest-reference example. **Severity: Block** — the cited exemplar does not demonstrate the claimed pattern.

---

### Missing nuance / drift [Warn]

- **"The registry GC will dedupe in the background."** This is imprecise on two counts. (1) Content-addressable deduplication in OCI registries is structural and immediate, not GC-driven: a push of a blob whose digest already exists typically completes instantly because the registry already has the bytes (the native transport's `blob_exists` check at `native_transport.rs:259` also skips re-upload from the client side). (2) GC in OCI registries reclaims unreferenced blobs, it does not deduplicate. The statement conflates two distinct mechanisms. More accurate: "The registry accepts duplicate uploads without error — most registries short-circuit the upload when the digest already exists — but the publisher still spends the upload bandwidth on the first push attempt." **Severity: Warn** — factually imprecise but not harmful to the workflow described.

- **Step 2: "OCX uploads it under the digest captured in step 1."** Slightly misleading: OCX computes the digest itself when reading the file (it does not use the sha256sum output as input). The `sha256sum` in step 1 is for the publisher to record the digest for later reuse; OCX independently hashes the file during upload (`client.rs:551–554`). The phrasing "under the digest captured in step 1" could imply the publisher supplies the digest to OCX during the file push, which is not the case. **Severity: Warn** — minor, but could confuse readers who wonder whether they need to pass the digest during the initial file push.

---

### Broken refs [Block]

- **`[mirror-cmake]` links to `https://github.com/ocx-sh/ocx/blob/main/mirrors/cmake/mirror.yml`.** The cmake mirror file exists locally at `mirrors/cmake/mirror.yml` and the link target is plausible, but the pattern is not visible in that file (see "Inconsistent / hallucinated" above). The link itself is structurally valid but the content it points to does not support the surrounding claim.

- **`[mirror-pipeline]` links to `https://github.com/ocx-sh/ocx/tree/main/mirrors`.** This points to the mirrors directory on GitHub. The associated claim "and the rest of the in-tree mirrors" implies these mirrors demonstrate digest layer reuse, which they do not (all use `LayerRef::File` only). The link destination exists but does not support the claim it illustrates.

---

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **Cast title mismatch (Warn).** The `.cast` file header at line 1 declares `"title": "Reusing a layer across packages"`. The `Terminal` component prop in the doc reads `title="Reusing a base layer across two releases"`. The component renders its own `title` prop in the chrome title bar (`Terminal.vue:183`), overriding the cast internal title, so the displayed title is correct. However the internal cast title is inconsistent — it lacks "base" and "two releases". This may confuse maintainers editing the recording. **Severity: Warn** — no user-visible impact.

- **Recording script uses `sha256sum` with `awk` pipe (Warn).** `test/recordings/scripts/package-layer-reuse.sh:7`: `BASE_DIGEST=$(sha256sum base.tar.xz | awk '{print $1}')` — this correctly captures the hex portion of sha256sum output. The result is then used as `sha256:${BASE_DIGEST}.tar.xz`. This matches the documented step-by-step workflow and correctly demonstrates the pattern. No issue with the logic. **Severity: none** — recording script is correct.

---

### Style / convention violations [Warn]

- **`sha256:<64-hex>.<ext>` fenced code block uses plain fence (no language tag).** The code block on lines 41–43 uses a bare triple-backtick fence. Per codebase patterns, short identifier-syntax examples should remain plain fences (no language tag applicable), so this is acceptable. **Severity: none** — acceptable for syntax illustration.

- **Missing `{#layer-reuse}` cross-reference from storage in-depth.** The `in-depth/storage.md#multi-layer` section (`storage.md:121–148`) discusses digest layer reuse inline and even has a `:::warning Bring your own archives` callout at lines 140–144 that repeats the `./` disambiguation tip nearly verbatim. These two sections are not cross-linked. **Severity: Warn** — duplication without cross-reference; readers who find storage.md first may not reach building-pushing.md.

---

## website/src/docs/authoring/bundle-anatomy.md#compression — Choosing the Compression

### Verified

- **`.tar.xz` is the default extension** — confirmed. `infer_filename()` in `crates/ocx_cli/src/command/package_create.rs:103` hardcodes `format!("{}.tar.xz", name)` as the fallback. The `CompressionLevel::Default` variant is the clap `#[default]` at `crates/ocx_cli/src/options/compression_level.rs:11`. Both independently confirm `.tar.xz` / LZMA as default.

- **Compression level flag values `fast` / `default` / `best`** — confirmed. `ocx package create --help` output shows `[possible values: fast, best, default]`. `CompressionLevel` enum in `crates/ocx_cli/src/options/compression_level.rs:8-12` defines exactly these three variants. Doc lists `fast`, `default`, `best` — CLI lists `fast, best, default` (alphabetical). The order differs but the values are correct.

- **Threads flag is `-j`** — confirmed. `package_create.rs:34` declares `#[arg(short = 'j', long, default_value_t = 0)]`.

- **`-j` only affects LZMA** — confirmed. `compression.rs:220–237` shows the match: `CompressionAlgorithm::Lzma if threads > 1` branches to `XzWriterMt`, and the single-thread Lzma path also exists; the `Gzip` arm calls `GzEncoder::new(output, level.into())` with no thread argument. The `threads` value is computed and available but never passed into the Gzip encoder.

- **`0` auto-detects** — confirmed. `CompressionOptions::threads_or_default()` at `compression.rs:138–145`: `if self.threads == 0 { default_threads() }`.

- **Cap is 16 cores** — confirmed. `default_threads()` at `compression.rs:79–82`: `.map(|n| (n.get() as u32).min(16))`. Doc says "up to 16 cores" — matches source exactly.

- **"download once and cache"** — confirmed at the implementation level. `pull.rs:249–262` shows Defense Layer 2: `find_plain()` checks for an already-installed package and returns early with `log::debug!("Package '{}' already fully installed, skipping.")`. Layer extraction also has a skip path (`pull.rs:653`). Blobs are content-addressed and shared across packages — a blob downloaded once lives in `blobs/` until GC'd.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **"20–40% smaller for binaries over Gzip"** — sourceless. No benchmark in the codebase (no bench/ directory, no test fixture measuring ratio). The range is plausible for LZMA vs Gzip on binary payloads but is an unverified marketing claim. Source: no file in `crates/` or `test/` confirms this figure. Mark **Unverified**; should either cite an external benchmark or soften to "typically smaller" with no specific range.

- **"single-digit percent on most binary payloads"** — unverifiable inline benchmark. No project-internal data. Technically plausible (LZMA default preset 3 vs best preset 9 does produce diminishing returns), but presented as fact without citation. Mark **Warn (vague claim, no citation)**.

- **"download once and cache" framing** — technically accurate but incomplete. The doc says "generated launcher scripts and CI checkout paths most users care about download once and cache". This omits the nuance that caching is _content-addressed_ and keyed by digest, not by tag. If a tag is updated and the index is refreshed, `ocx install` will download a new digest even for the same package name. The sentence will mislead users who interpret "download once" as "forever, regardless of updates". Nudge: add "for the same digest" qualifier or link to the layer reuse / caching docs.

- **No mention that `-j` default is `0` (auto-detect), not 1** — the help text says `[default: 0]`, which means auto-detect on every system. The doc only says "`0` auto-detects up to 16 cores" without noting that 0 is the default. A user reading the doc may not realize `-j` already defaults to parallel. Low severity but could cause confusion if a user believes serial compression is the default.

### Broken refs [Block]

- (none) — no links in this subsection.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none) — no code samples in this subsection.

### Style / convention violations [Warn]

- **Missing `:::tip` callout for the "Stick with `default` until a profiler tells you otherwise" recommendation** — `docs-style.md` states `:::tip` is for "Actionable advice, example usage, recommended patterns". The sentence "Stick with `default` until a profiler tells you otherwise" is exactly this pattern and is currently buried in prose. Per the style guide it should be a `:::tip` block. Mark **Warn**.

- **"single-digit percent" claim** (style overlap) — beyond being unverifiable, the wording is vague prose in a technical reference section. If retained, rephrase with a concrete qualifier or move to a `:::details` callout labelled "Compression ratio notes" so it is clearly informational rather than authoritative.

---

**Evidence index:**
| Claim | Source |
|---|---|
| Default extension `.tar.xz` | `crates/ocx_cli/src/command/package_create.rs:103` |
| Compression level values fast/default/best | `crates/ocx_cli/src/options/compression_level.rs:8-12`; `ocx package create --help` |
| `-j` flag exists | `crates/ocx_cli/src/command/package_create.rs:33-35` |
| `-j` only affects LZMA | `crates/ocx_lib/src/compression.rs:220-237` |
| `0` auto-detects | `crates/ocx_lib/src/compression.rs:138-145` |
| Cap at 16 | `crates/ocx_lib/src/compression.rs:79-82` |
| Download-once caching | `crates/ocx_lib/src/package_manager/tasks/pull.rs:249-262` |
| 20-40% LZMA claim | No source found — Unverified |
| single-digit % claim | No source found — Warn |
| `:::tip` guidance | `.claude/rules/docs-style.md:95` |

---

## website/src/docs/authoring/bundle-anatomy.md#sidecars — Sidecar Metadata and Inferred Names

### Verified

1. **`-m` copies sidecar to `<archive-stem>-metadata.json`** — confirmed.
   `conventions.rs:10–27`: `infer_metadata_file` calls `Path::file_stem()` (strips last extension,
   so `.tar.xz` → stem `<name>.tar`), then strips the `.tar` suffix via the `known_archive_extensions`
   list, yielding `<name>`. Result: `<name>-metadata.json`. For `mytool-1.0.0.tar.xz` the output is
   `mytool-1.0.0-metadata.json`. Correct.

2. **`ocx package push` omitted `-m` looks for `<stem>-metadata.json` next to the first file layer** —
   confirmed. `package_push.rs:65–76`: finds first `LayerRef::File`, calls `conventions::infer_metadata_file`
   on that path. Same function as create side. Correct.

3. **`-i mytool:1.0.0 -p linux/amd64 -o .` produces `mytool-1.0.0-linux-amd64.tar.xz`** — confirmed.
   `package_create.rs:90–104`: `infer_filename` formats `{identifier.name()}-{tag_or_latest()}` then
   appends `platform.ascii_segments().join("-")`, then `.tar.xz`. For `mytool:1.0.0` + `linux/amd64`
   → `mytool-1.0.0-linux-amd64.tar.xz`. Correct.

4. **Default compression extension `.tar.xz` when inferred via `-o .`** — confirmed.
   `package_create.rs:103`: `infer_filename` hard-codes `format!("{}.tar.xz", name)` — always LZMA.
   The BundleBuilder doc comment (`bundle.rs:14–16`) confirms `.tar.xz` selects LZMA, and labels it
   "the default when the filename is inferred". Correct.

5. **`-i` flag for identifier on `ocx package create`** — confirmed. `package_create.rs:17–18`:
   `#[clap(short, long)] identifier: Option<options::Identifier>`. Help output confirms `-i, --identifier`.

6. **`-p` flag for platform on `ocx package create`** — confirmed. `package_create.rs:19–20`:
   `#[clap(short, long)] platform: Option<oci::Platform>`. Help output confirms `-p, --platform`.

7. **`<Tree :collapsible="false">` prop syntax** — confirmed. `Tree.vue:11`:
   `collapsible?: boolean`, default `true`. `:collapsible="false"` is correct Vue syntax
   for binding a boolean `false` literal. Subsystem rule matches.

8. **`[cmd-package-create]` → `../reference/command-line.md#package-create`** — confirmed.
   `command-line.md:773`: `#### \`create\` {#package-create}`. Anchor exists.

9. **`[cmd-package-push]` → `../reference/command-line.md#package-push`** — confirmed.
   `command-line.md:830`: `#### \`push\` {#package-push}`. Anchor exists.

10. **`[authoring-multi-platform]` → `./multi-platform.md`** — confirmed. File exists at
    `website/src/docs/authoring/multi-platform.md`. Link def at `bundle-anatomy.md:118`.

11. **`[in-tree-mirrors]` → `https://github.com/ocx-sh/ocx/tree/main/mirrors`** — confirmed.
    `bundle-anatomy.md:100`. Local `mirrors/` directory exists with cmake, bun, etc. Link def correct.

12. **`-m` is the short flag for `--metadata` on both `create` and `push`** — confirmed.
    `package_create.rs:27–29` and `package_push.rs:28–31`. Both use `#[clap(short, long)]`
    which generates `-m` as the short form. Doc examples use `-m metadata.json` correctly.

### Inconsistent / hallucinated [Block]

1. **Pattern description says `<repo>-<tag>-<os>-<arch>` but code uses `<name>`** (last segment only).
   `package_create.rs:92`: `identifier.name()` not `identifier.repository()`.
   `identifier.rs:146–148`: `name()` returns `self.repository.rsplit('/').next()` — i.e., the last
   segment of the repository path. For `myorg/mytool:1.0.0` the inferred filename would be
   `mytool-1.0.0-linux-amd64.tar.xz`, NOT `myorg-mytool-1.0.0-linux-amd64.tar.xz`.
   The doc says "The pattern is `<repo>-<tag>-<os>-<arch>.tar.xz`" — this is misleading because
   `<repo>` implies the full repository path, but the implementation only uses the last segment
   (the package name). For simple single-segment repos like `mytool:1.0.0` the example is correct,
   but the stated pattern is inaccurate for scoped repos (`myorg/mytool`). Should read
   `<name>-<tag>-<os>-<arch>.tar.xz` where `<name>` = last path segment of the repository.

2. **`<Description>` with `<code>` element is silently truncated** — the doc uses:
   ```
   <Description>sidecar — same bytes as the input <code>metadata.json</code></Description>
   ```
   `Tree.vue:19–27`: `descText()` maps child VNodes via `typeof v.children === 'string' ? v.children : ''`.
   A `<code>` VNode's `children` is a slot object, not a string, so it yields `''`. The rendered
   description becomes `"sidecar — same bytes as the input "` — the `metadata.json` text is dropped.
   The `FileTreeNode.vue:93` renders description as plain text (`{{ node.description }}`), not
   `v-html`, so HTML elements in `<Description>` slots are not supported. Backtick-wrapped plain text
   is the only way to render inline code in descriptions (though it won't style as `<code>` either).

### Missing nuance / drift [Warn]

1. **"compression extension follows whichever filename you specify" is only partially true.**
   When `-o` specifies an explicit output filename (e.g., `-o mytool-1.0.0.tar.gz`), the extension
   IS used by `BundleBuilder` to select the codec (`bundle.rs:14–16`). But when `-o .` triggers
   filename inference, `infer_filename` hard-codes `.tar.xz` (`package_create.rs:103`) — the user
   cannot influence the extension via the identifier or platform flags. So the phrase "follows
   whichever filename you specify" only applies to the explicit-filename path, not the inferred path.
   The section describes the inferred-name case (`-o .`) but applies the phrase broadly. Should
   clarify: "when providing an explicit filename, the extension selects the codec; when inferring
   via `-o .`, `.tar.xz` (LZMA) is always used."

2. **`known_archive_extensions` list in `infer_metadata_file` does NOT include `.tar.xz`.**
   `conventions.rs:19`: `[".tar", ".tar.gz", ".tgz", ".zip"]`. For `.tar.xz` files, the double-extension
   stripping works via `file_stem()` (strips `.xz`) then the list matches `.tar`. This is correct
   behavior, but if someone uses a non-standard extension (e.g., `.txz`), `file_stem()` returns
   `name.txz`, which does not match any extension in the list, so the metadata filename would be
   `name.txz-metadata.json` rather than `name-metadata.json`. The doc does not mention this edge
   case, but `.txz` is a supported push layer extension (`package_push.rs` comment lists `txz`).
   Minor inconsistency between what create produces and what push accepts as layer format.

3. **Section does not mention that `-m` also validates the metadata** before copying.
   `package_create.rs:80–81`: `Metadata::read_json` + `ValidMetadata::try_from` run before copy.
   An invalid `metadata.json` causes the command to fail without producing a sidecar. The doc
   implies it's a pure copy operation. Not a blocker, but useful nuance for authors.

### Broken refs [Block]

- (none) — all four reference links verified present and correctly targeted.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none) — the two shell examples are accurate for single-segment repository names. The sidecar
  filenames shown in the `<Tree>` blocks match what `infer_metadata_file` and `infer_filename`
  produce for the given inputs. The cast `/casts/package-create.cast` exists in
  `website/src/public/casts/`. No JSON samples in this section.

### Style / convention violations [Warn]

1. **`<Description>` slots with `<code>` elements will silently lose the inline code text**
   (same as Block finding #2 above, repeated here for style tracking): use plain backticks
   in description text or restructure prose. This is both a functional bug and a style issue.

2. **Shell command blocks at lines 48–50 and 65–67 use plain ` ```sh ` fences, not `<Terminal>`.**
   The `docs-style.md` component reference and site-wide pattern (see `building-pushing.md`,
   `multi-platform.md`, `migration.md`) use `<Terminal src="...cast" ... />` for full command
   demonstrations and plain ` ```sh ` fences only for short illustrative snippets.
   The `<Terminal src="/casts/package-create.cast" ... />` at line 36 covers the `create` flow,
   and there is no dedicated recording for the inferred-name sidecar pattern specifically.
   The current pattern (cast for the full demo, fences for the two short concept illustrations)
   is consistent with how `multi-platform.md:24–28` handles the same pattern. No change required
   unless a dedicated sidecar recording is created.

3. **Compression section (line 40) and Sidecars section (line 44) both refer to `.tar.xz` as
   "default" without cross-linking.** Minor navigation debt — not a violation.

---

## website/src/docs/authoring/bundle-anatomy.md#stable — Stable Archives

### Verified

- **Sorts entries by filename** (claim 1): Confirmed. `crates/ocx_lib/src/archive/tar.rs:105` — `entries.sort_by_key(|e| e.file_name())`. The `add_dir_recursive` function collects all `read_dir` entries, sorts them by `OsStr` file name, then appends each to the builder. The ZIP backend has the same sort (`zip.rs:151`). Claim is accurate.

- **mtimes read from filesystem and embedded** (claim 2): Confirmed — and **no mtime normalization** occurs in production paths. `crates/ocx_lib/src/archive/tar.rs:117` calls `builder.append_path_with_name(&path, &archive_name)` which, per the `tar` crate, reads the file's metadata (including `mtime`) and writes it verbatim into the tar header. The only `set_mtime(0)` call in the codebase is at `crates/ocx_lib/src/archive.rs:518` inside a **test fixture** that crafts a path-traversal archive — it is never reachable from `BundleBuilder`. Claim is accurate.

- **`-n` / `--new` flag on `ocx package push`** (claim 3): Confirmed. `crates/ocx_cli/src/command/package_push.rs:23-24` — `#[clap(long = "new", short = 'n')]`. The flag's purpose (skip pre-push tag listing for new packages) matches the doc description.

- **`sha256:<hex>.<ext>` form on `ocx package push`** (claim 4): Confirmed. `crates/ocx_lib/src/publisher/layer_ref.rs` fully documents and implements the `LayerRef::Digest` parsing. `FromStr` accepts `sha256:<64-hex>.tar.gz`, `.tgz`, `.tar.xz`, `.txz`; `.tar.gz` and `.tar.xz` are the canonical extensions; aliases `tgz`/`txz` also accepted. The doc uses `.tar.xz` in context — correct. Note: the code at `layer_ref.rs:147-177` shows that `sha256:` prefix is required only for the 64-char hex; other algorithm prefixes (`sha512:`) also parse, but the doc's `sha256:${BASE_DIGEST}` form is the canonical user-facing pattern and is accurate.

- **"local sha256 matches the layer digest the registry will hold"** (claim 5): Confirmed. `crates/ocx_lib/src/oci/client.rs:551-558` — for `LayerRef::File`, the push path calls `Algorithm::Sha256.hash_file_read(path)` which reads the `.tar.xz` file from disk and computes its SHA-256 over the raw (compressed) bytes. The resulting digest is used directly as the OCI blob descriptor digest. `sha256sum` on the local file produces exactly that same hash. Claim is accurate.

- **`[in-depth-storage-layers]` anchor** (claim 7): Confirmed. `website/src/docs/in-depth/storage.md:96` — `## Layers {#layers}` is present and has substantive content.

- **`[authoring-layer-reuse]` reference** (claim 8): Confirmed. `website/src/docs/authoring/building-pushing.md:35` — `## Reusing Layers Across Packages {#layer-reuse}` exists. The link definition at `bundle-anatomy.md:116` maps `[authoring-layer-reuse]` to `./building-pushing.md#layer-reuse`. Both anchor and content are present.

- **Style — phrase "is not byte-reproducibility — it is to capture the digest"** (claim 10): Clear and non-marketing. No issues.

- **`<Terminal src="/casts/package-create.cast" />` — cast file exists** (claim 6, existence check): Confirmed. `website/src/public/casts/package-create.cast` exists (814B). The recording shows `ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz` followed by a "Finishing" spinner — matches the code example in this subsection exactly. No claims contradicted by cast content.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **Sorting is by `OsStr` (byte order), not Unicode/locale filename** (claim 1 nuance): The doc says "sorts entries by filename" — technically correct, but the sort key is `entry.file_name()` returning `OsStr`, which is byte-ordered on Linux/macOS and UTF-16-code-unit-ordered on Windows. Files with non-ASCII names sort differently across OSes. This is a minor nuance: the claim as stated is not wrong, but "sorts by filename" could be misread as a locale-aware or Unicode-normalized sort. The doc's purpose is to say iteration order doesn't leak in — that intent is fully achieved regardless. Low-priority clarification candidate.

- **"two downloads at install time" overstatement** (claim 11 nuance, "zero dedup"): The phrase "two downloads at install time" implies a consumer who installs both pushes will download the layer twice. This is only true if the consumer installs packages backed by both registrations before either is GC'd. In practice: the registry will store two distinct blobs (two different SHA-256 digests, from two separate bundle runs), so downloads are indeed two separate blobs and there is no automatic deduplication at the registry layer for distinct digests. The claim "zero dedup" is accurate for the non-reproducible-bundle scenario. The `building-pushing.md` page (line 141) adds a nuance — "the registry GC will dedupe in the background" — but this applies only to deduplication of identical digests, not to the distinct-digest case described here. For the scenario under discussion (two different bundle runs → two different digests), "zero dedup" at the registry level is correct. No factual error; the phrase is concise and correct in context.

- **Compression non-determinism not mentioned**: The doc attributes non-reproducibility to mtimes (and implicitly iteration order), but another source of variation is LZMA compression itself: on some xz implementations, thread count, encoder state, or dictionary choices can affect output bytes even with identical inputs and mtimes. This is subtle and implementation-specific; not a factual error in the doc, but completeness gap for sophisticated readers.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **Cast content vs. subsection scope** [Warn]: The `package-create.cast` recording shows only the command and a "Finishing" spinner — no output lines confirming what was created, no follow-up `sha256sum` or `ocx package push`. The cast demonstrates that `ocx package create` runs, but does not demonstrate the stable-archives workflow described in the surrounding prose (capture digest, reuse by digest). The cast at `website/src/public/casts/package-layer-reuse.cast` (5.2K) is more relevant to the complete pattern described here and is referenced from `building-pushing.md#layer-reuse`. This is a weak match: the cast is technically correct (it shows bundling) but thin for the subsection's teaching goal.

### Style / convention violations [Warn]

- **Code fence vs. `<Terminal>` pairing** (claim 9): `docs-style.md` and `subsystem-website.md` document `<Terminal>` with inline `<Frame>` elements for interactive demos and `<Terminal src="...">` for recorded casts. The current structure uses both a static code fence (lines 22-32) AND a `<Terminal src="/casts/package-create.cast">` (line 36). The code fence is the canonical reference for the pattern; the `<Terminal>` supplies the visual recording. This pairing is used elsewhere in the docs (e.g. `building-pushing.md` has static code + `<Terminal src="...">`) and is acceptable per convention — no mechanical violation detected. No mandatory change required.

---

## website/src/docs/authoring/bundle-anatomy.md#strip-components — Stripping Upstream Wrappers

### Verified

- **`strip_components` is a top-level field of `Bundle`.**
  Confirmed in `crates/ocx_lib/src/package/metadata/bundle.rs:51`:
  `pub strip_components: Option<u8>` is a direct field on `Bundle`, not nested.
  Also confirmed in `website/src/public/schemas/metadata/v1.json` (`$defs/Bundle.properties.strip_components`)
  and the reference doc table at `website/src/docs/reference/metadata.md:35`.

- **Default value when omitted is `0` (no stripping).**
  `bundle.rs:50`: `#[serde(skip_serializing_if = "Option::is_none", default)]` — field defaults to `None` on
  deserialization when absent.
  At extraction time, `oci/client.rs:417`: `strip_components: bundle.strip_components.unwrap_or(0).into()`.
  Reference doc `metadata.md:375` table row: `omitted / 0 → Extract as-is.` Consistent across all three.

- **"Removes that many leading path segments at install time" — verified.**
  Extraction happens during layer pull in `oci/client.rs:415–420` (not at symlink-creation install time,
  but during the pull/extract phase which is part of the install pipeline).
  Implementation in `archive/tar.rs:147`: `path.iter().skip(strip_components).collect()` — skips N leading
  components per entry. Same logic in `archive/zip.rs:246`. `ExtractOptions.strip_components: usize` is
  passed directly from `bundle.strip_components.unwrap_or(0)`.

- **Example path transform `cmake-3.28/bin/cmake` → `bin/cmake` at value `1` is correct.**
  Confirmed by the tar extraction: skipping 1 component drops `cmake-3.28/`, leaving `bin/cmake`.
  Reference doc `metadata.md:376` has the same example: `cmake-3.28/bin → bin`.

- **`[strip-components]` link definition (`bundle-anatomy.md:108`) resolves correctly.**
  Defined as `../reference/metadata.md#extraction-strip-components`.
  Anchor `#extraction-strip-components` exists at `metadata.md:367`: `### \`strip_components\` {#extraction-strip-components}`.

- **JSON sample fields `type`, `version`, `strip_components` are all valid fields.**
  Schema confirms: `type` required at root (via `oneOf` + `required: ["type"]`, `metadata.md:240`),
  `version` required in `Bundle` (`required: ["version"]`), `strip_components` optional integer. All
  three are present and correct in the sample.

- **No `name` required field missing from sample.**
  `Bundle` struct has no `name` field. The schema has no `name` at the metadata level.
  The sample `{ "type": "bundle", "version": 1, "strip_components": 1 }` is a **valid** minimal document
  under the schema.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **"Interaction with multi-layer packages" promised by `bundle-anatomy.md:82` is absent from the link target.**
  The doc states: "The mechanic, edge cases, and **how it interacts with multi-layer packages** live in
  the [metadata reference][strip-components]."
  The actual anchor `#extraction-strip-components` (`metadata.md:367–378`) contains only a 9-line table
  — no prose on multi-layer interaction, no edge cases, no explanation of how stripping applies across
  multiple OCI layers. The cross-reference over-promises: the reference doc does not cover the promised
  content. The implementation applies `strip_components` per-layer at extraction time
  (`oci/client.rs:415–420`), meaning each layer is stripped independently before content is assembled —
  this nuance is undocumented.

- **"At install time" is imprecise.**
  The doc says "OCX removes that many leading path segments **at install time**." Technically, stripping
  happens during **layer extraction** (inside `pull_layer`, called from `extract_layer_inner` at
  `tasks/pull.rs:739`), not during the symlink-creation step (`tasks/install.rs`). For most users the
  distinction is invisible, but technically "during download/extraction" is more accurate than "at install
  time." Low-risk imprecision; no user confusion expected.

### Broken refs [Block]

- (none)

  The anchor `#extraction-strip-components` exists and resolves. The link definition on line 108 is
  syntactically correct. No broken links detected.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **[Warn] JSON sample omits `$schema`.**
  Every other example in `metadata.md` (lines 16–21, 62–100, 207–226) includes the
  `"$schema": "https://ocx.sh/schemas/metadata/v1.json"` field for editor autocompletion.
  The sample in `bundle-anatomy.md:84–89` omits it. The sample is valid without it (schema field is not
  required), but inconsistent with the convention established by the reference doc. Weak presentation.

### Style / convention violations [Warn]

- **[Warn] `tar --strip-components` mentioned in inline code without hyperlink.**
  `docs-style.md` §"Real-World Examples and External Links" (line 50): "Every external tool mentioned
  must hyperlink — every occurrence, not just first."
  `bundle-anatomy.md:82` writes `` `tar --strip-components` `` as inline code only. GNU tar (or
  man page equivalent, e.g. https://www.gnu.org/software/tar/manual/tar.html) is not linked.
  No link definition exists in the file for `tar` or `gnu-tar`.
  Classification: Warn (style convention violation; no correctness impact).

---

## website/src/docs/authoring/bundle-anatomy.md#what-goes-in — What Goes in the Archive

### Verified

- **`strip_components` is a top-level field on `bundle` metadata**: Confirmed in `crates/ocx_lib/src/package/metadata/bundle.rs:51` (`pub strip_components: Option<u8>`) and documented at `website/src/docs/reference/metadata.md:35` in the top-level structure table.
- **`strip_components` anchor resolves**: `[strip-components]: ../reference/metadata.md#extraction-strip-components` and the anchor `{#extraction-strip-components}` exists at `website/src/docs/reference/metadata.md:367`. Valid.
- **`migration patterns guide` link resolves**: `[authoring-migration]: ./migration.md` — file exists at `website/src/docs/authoring/migration.md` and its content (Homebrew, GitHub Releases, `ocx_mirror` pipeline) matches the claim "walks the most common transformations". No anchor required (top-of-page link). Valid.
- **cmake archive naming pattern**: `mirrors/cmake/mirror.yml:14` regex `cmake-.*-linux-x86_64\.tar\.gz` (>= 3.20.0) confirms upstream CMake ships archives whose top-level directory follows the pattern `cmake-3.28.1-linux-x86_64/` (one wrapper dir, hence `strip_components: 1` in `mirrors/cmake/mirror.yml:32`). The doc's example `cmake-3.28.1-linux-x86_64/bin/cmake` is accurate for the >= 3.20.0 naming convention.
- **Node.js archive naming pattern**: `mirrors/nodejs/mirror.yml:13` regex `node-.*-linux-x64\.tar\.xz` confirms the upstream tarball naming. The Node.js project uses root dir `node-v20.0.0-linux-x64/` inside the archive (standard upstream layout). Example `node-v20.0.0-linux-x64/bin/node` is accurate.
- **PATH variables are prepended**: Confirmed in `crates/ocx_lib/src/package/metadata/env/path.rs` (path type entries are prepended) and `website/src/docs/reference/metadata.md:118–119` ("Path variables are **prepended** to any existing value"). `crates/ocx_lib/src/package_manager/composer.rs:391–397` confirms prepend semantics.
- **`ocx_mirror` verifies GitHub asset digests**: `crates/ocx_mirror/src/pipeline/verify.rs:89–93` verifies downloaded assets against `github_asset_digest` when configured. `mirrors/cmake/mirror.yml:51` and `mirrors/nodejs/mirror.yml` (no verify section, defaults to `github_asset_digest: true` per `verify_config.rs:8`). Verification happens on download, before rebundling.
- **`content/` is the post-install directory name**: Confirmed at `crates/ocx_lib/src/file_structure/package_store.rs:11,39` and `storage.md:54`. `content/` is the OCX-managed directory inside the package store, not a publisher-side archive layout requirement.
- **Link syntax**: All links in the section use reference-style syntax (no inline `[text](url)` in body prose). Compliant with `docs-style.md`.

### Inconsistent / hallucinated [Block]

- **Claim "preserves the upstream digest as a verification anchor"** (`bundle-anatomy.md:12`): This claim is misleading in context. The doc says "Repackaging upstream archives unchanged [...] preserves the upstream digest as a verification anchor". This implies a tool-level guarantee. In reality: (1) `ocx_mirror` always extracts and **rebundles** upstream archives (`crates/ocx_mirror/src/pipeline/package.rs:21–53`), so the upstream digest is NOT preserved in the OCI layer for in-tree mirrors; (2) if a manual publisher pushes an upstream archive to OCX unchanged via `ocx package push`, the OCI layer SHA-256 will happen to equal the archive SHA-256, but OCX has no mechanism to record, expose, or verify this correspondence as an "anchor" — no referrer, no metadata field, no cli command checks it. The claim elevates a coincidence of SHA-256 bytes into a named feature that does not exist in the toolchain. **Fix**: Remove or qualify the "verification anchor" phrase. The accurate benefit of not repackaging is avoiding extra build steps, not a digest-verification feature.

- **Claim "every in-tree mirror follows: the archive's content lives under a `content/` view that contains `bin/`"** (`bundle-anatomy.md:14`): `content/` is an OCX **install-time** storage directory (see `crates/ocx_lib/src/file_structure/package_store.rs:11,54`), not a publisher-side archive layout convention. Publishers do not create `content/` in their archives — OCX creates it on the filesystem. Several in-tree mirrors (`mirrors/jfrog-cli/`, `mirrors/shfmt/`) are `type: binary` (no archive at all, no `bin/` directory). `mirrors/lychee/mirror.yml:34` uses `strip_components: 0` — the binary lands at the archive root, not under `bin/`. `mirrors/go-task/`, `mirrors/oras/` have no `asset_type` block, defaulting to `Archive { strip_components: None }` (`crates/ocx_mirror/src/command/sync.rs:192`), meaning no strip and no prescribed layout. The claim "every in-tree mirror" is false: at minimum lychee, jfrog-cli, shfmt, and oras do not match the described `bin/`-centric shape. **Fix**: Describe `content/` as the OCX post-install directory name, and drop "every in-tree mirror follows" unless narrowed to "most archive-type mirrors that use strip_components: 1".

### Missing nuance / drift [Warn]

- **`ocx exec` "prepend bin/ to PATH" framing** (`bundle-anatomy.md:10`): The doc says `ocx exec` can "prepend `bin/` to `PATH` without knowing the upstream's naming convention." This understates the mechanism. `ocx exec` does not hardcode `bin/` — it resolves env entries from the package's `metadata.json` (`exec.rs:64`). The PATH entry is only present if the publisher declared `{"key":"PATH","type":"path","value":"${installPath}/bin"}` in their metadata. A package with no `env` declaration exports nothing to PATH. The framing implies `ocx exec` is the agent; the correct agent is `metadata.json` + `strip_components`. Consider: "so the package's declared `bin/` entry can prepend to `PATH`..."
- **"lib/" presented as a standard optional component**: The doc says "optional `lib/` (libraries)" as part of the in-tree convention. In practice, no in-tree mirror declares a `lib/` env entry or depends on a `lib/` subtree in metadata. `mirrors/mold/README.md` mentions `lib/mold/mold-wrapper.so` but `mirrors/mold/metadata.json` only declares PATH. The `lib/` framing is not wrong but is not grounded in any in-tree example.
- **cmake version example in `#strip-components`** (`bundle-anatomy.md:82`): "collapses `cmake-3.28/bin/cmake` to `bin/cmake`" — this uses an abbreviated version `cmake-3.28` rather than the full three-component `cmake-3.28.1-linux-x86_64` used in the intro (line 10). The abbreviated form is acceptable as an illustration but slightly inconsistent with the detailed example above.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **JSON example at #strip-components** (`bundle-anatomy.md:84–90`): `{"type":"bundle","version":1,"strip_components":1}` is valid against the Rust types (`crates/ocx_lib/src/package/metadata/bundle.rs`). Correct.

### Style / convention violations [Warn]

- **"verification anchor"** is jargon that could qualify as a `<Tooltip>` candidate per `docs-style.md` ("Good candidates: technical terms, jargon, protocol-level concepts"). However, since the claim itself is factually inconsistent (see Block above), the priority is fixing the claim, not wrapping it in a tooltip.
- **"in-tree mirror"** appears without a tooltip or link definition inline. The page already links `[in-tree-mirrors]` at line 100 (`https://github.com/ocx-sh/ocx/tree/main/mirrors`), but that link is only used in `#sidecars` (line 78), not in `#what-goes-in`. Adding a reference or tooltip in line 14 where "in-tree mirror" is first used would improve clarity per `docs-style.md`.
- Style is otherwise consistent with `docs-style.md`: short paragraphs, no inline links in body prose, reference-style links collected at page bottom, no marketing opens.

---

## website/src/docs/authoring/dependencies.md#edge-visibility — Choosing Edge Visibility

### Verified

- **`sealed` as default** — `Visibility::default()` derives `Default`, struct zero-value is `{private: false, interface: false}` which equals `Visibility::SEALED`. `Dependency.visibility` uses `#[serde(default)]`, so absent field deserialises to `SEALED`. Confirmed by unit test `dependency_omitted_visibility_defaults_to_sealed` at `crates/ocx_lib/src/package/metadata/dependency.rs:313`. Claim correct.

- **Four values exact: `sealed`/`private`/`public`/`interface`** — `Visibility` custom `Serialize`/`Deserialize` at `crates/ocx_lib/src/package/metadata/visibility.rs:157–188` matches exactly those four lowercase wire strings. The JSON Schema in `Visibility::json_schema` lists `["sealed", "private", "public", "interface"]` (line 199). Claim correct.

- **Two-axis model `private` + `interface`** — struct fields named `private: bool` and `interface: bool` at `visibility.rs:77–82`. Doc text says "private = the package's own runtime sees it; interface = consumers see it". Code comment at line 68 confirms: "`private` (true, false), `interface` (false, true), `public` (true, true)". Axis naming and semantics verified.

- **`sealed` table row** — doc: `private=No, interface=No`. Code: `SEALED = {private:false, interface:false}` (visibility.rs:87–90). Correct.

- **`private` table row** — doc: `private=Yes, interface=No`. Code: `PRIVATE = {private:true, interface:false}` (visibility.rs:93–97). Correct.

- **`public` table row** — doc: `private=Yes, interface=Yes`. Code: `PUBLIC = {private:true, interface:true}` (visibility.rs:99–104). Correct.

- **`interface` table row** — doc: `private=No, interface=Yes`. Code: `INTERFACE = {private:false, interface:true}` (visibility.rs:106–111). Correct.

- **`interface` semantics: "Meta-packages that forward env to consumers without using it themselves"** — code doc comment at visibility.rs:107 reads: "Typical for meta-packages that compose environments." Unit test at visibility.rs:468–479 (`interface_visibility_has_non_empty_interface_surface`) confirms `has_interface()=true` and `has_private()=false`. Semantics verified.

- **`through_edge` term** — `Visibility::through_edge` is a real method at `visibility.rs:141`. It is referenced in the in-depth environments doc at `website/src/docs/in-depth/environments.md:87` and the reference/metadata.md details block at line 250–255 (`#dependencies-through-edge`). Term is real, not invented.

- **`resolve.json` artifact** — real file written at install time. `pull.rs` line 544: "Writes the `resolve.json` metadata file with the resolved dependencies for this package." Referenced in `in-depth/environments.md:79` ("stored in `resolve.json`") and `reference/metadata.md:269` ("stored in `resolve.json`"). The authoring page at line 69 says it "shows up in `resolve.json`" — this is accurate; `reference/metadata.md:267–269` explicitly says diamond merge result "is computed at install time and stored in `resolve.json`."

- **`[in-depth-dependencies]` link** — defined at `authoring/dependencies.md:98` as `../in-depth/dependencies.md`. File `/home/mherwig/dev/ocx/website/src/docs/in-depth/dependencies.md` exists and has content on diamond deps, visibility, GC.

- **`[authoring-env-surface]` link** — defined at `authoring/dependencies.md:102` as `./env-surface.md`. File `/home/mherwig/dev/ocx/website/src/docs/authoring/env-surface.md` exists with visibility content distinct from dep-edge visibility (entry-level visibility).

- **`[cmake-tll]` external link** — defined at `authoring/dependencies.md:90` as `https://cmake.org/cmake/help/latest/command/target_link_libraries.html`. This is the canonical stable CMake documentation URL for `target_link_libraries`. URL format matches CMake docs conventions.

- **`[mise]` external link** — defined at `authoring/dependencies.md:91` as `https://mise.jdx.dev/`. Correct mise homepage URL.

- **`:::info` callout for CMake analogy** — `docs-style.md` §"Callout Boxes" states `:::info` is for "Analogies to other systems, background context". The callout is an analogy. Use is compliant.

- **Table format** — standard GFM table, four values, three Boolean columns plus use-case. Clear and tabular.

- **Precision of opinion guidance** — "the default — `sealed` — is the right pick for most dependencies" is publisher-facing guidance in an authoring guide. This is appropriate opinion in context; the code comment at `visibility.rs:85–86` says "Most deps in a tool-focused package manager." Consistent with code commentary.

- **`:::info` callout disclaimer** — "Use the analogy as a memory aid; the behaviours are not identical." Correct caveat; CMake propagation is at build time / link time, OCX is runtime env. Precision appropriate.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **Doc says `resolve.json` is in scope of `[in-depth-dependencies]`** (line 69: "what shows up in `resolve.json` — lives in [dependencies in depth]"), but `in-depth/dependencies.md` does **not** mention `resolve.json` at all. The actual explanation of `resolve.json` and what it contains lives in `in-depth/environments.md:79` and `reference/metadata.md:267–270`. The pointer is misleading — a user following the link will not find the claimed content. [Warn]

- **Column header asymmetry between authoring and reference tables** — `authoring/dependencies.md:60` table uses "Private surface" and "Interface surface" as column headers, while `reference/metadata.md:239` uses "Private surface (`--self`)" and "Interface surface (default)". The authoring table omits the CLI flag hint, making it harder for readers to connect column semantics to the `--self` flag mentioned in the deeper docs. Low severity but a minor drift. [Warn]

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none — section contains no JSON samples)

### Style / convention violations [Warn]

- (none — link syntax uses reference-style as required, callout type correct, no inline `[text](url)` detected in body)

---

## website/src/docs/authoring/dependencies.md#name-field — When You Need a `name` Override

### Verified

- **`name` field exists on `Dependency` struct.**
  `crates/ocx_lib/src/package/metadata/dependency.rs:105-106` declares `pub name: Option<DependencyName>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. The field is real.

- **Default derivation is repository basename.**
  `dependency.rs:114-123` — `Dependency::name()` returns the explicit `name` when set, otherwise falls back to `self.identifier.name()`. `identifier.rs:146-148` defines `Identifier::name()` as `self.repository.rsplit('/').next()` — the last segment of the repository path. `ocx.sh/cmake` → `"cmake"`, `myorg/cmake` → `"cmake"`. This matches the prose.

- **Regex `^[a-z0-9][a-z0-9_-]*$` is exactly correct.**
  `crates/ocx_lib/src/package/metadata/slug.rs:11`:
  ```
  pub const SLUG_PATTERN_STR: &str = r"^[a-z0-9][a-z0-9_-]*$";
  ```
  The doc's claim is byte-for-byte identical.

- **64-character limit is exactly correct.**
  `slug.rs:12`: `pub const SLUG_MAX_LEN: usize = 64;`
  `dependency.rs:31`: validation checks `value.len() > SLUG_MAX_LEN`.

- **"Same rule as entry-point names" is correct.**
  `entrypoint.rs:8` imports the same `SLUG_MAX_LEN` and `SLUG_PATTERN` from `slug.rs`.
  `entrypoint.rs:28`: `pub const MAX_LEN: usize = SLUG_MAX_LEN;`
  `entrypoint.rs:39-45`: validates using `Self::MAX_LEN` and `SLUG_PATTERN`.
  Both newtypes (`DependencyName`, `EntrypointName`) share the same slug module — the claim is verified.

- **`name` field appears in JSON Schema.**
  `website/src/public/schemas/metadata/v1.json:63-73` — the `Dependency` definition includes a `name` property with `anyOf: [{ "$ref": "#/$defs/DependencyName" }, { "type": "null" }]`. The `DependencyName` definition at line 85-89 carries `"pattern": "^[a-z0-9][a-z0-9_-]*$"` and `"maxLength": 64`.

- **Tag is optional in `PinnedIdentifier`.**
  `pinned_identifier.rs:114` (schema description): `'registry/repository[:tag]@digest'` — tag is marked optional with `[...]`.
  Test `dependency_without_tag` at `dependency.rs:355-359` explicitly proves `"ocx.sh/java@sha256:..."` (no tag) deserializes successfully with `tag() == None`. The tag is advisory/optional.

### Inconsistent / hallucinated [Block]

- **Example identifiers `myorg/cmake@sha256:...` lack an explicit registry — and the code rejects them.**

  The section shows:
  ```json
  { "identifier": "myorg/cmake@sha256:...", "name": "myorg_cmake" }
  ```
  `myorg` contains no `.` or `:` and is not `"localhost"`, so `has_explicit_registry()` in `identifier.rs:281-287` returns `false`. `Identifier::parse()` (line 55-64) then returns `IdentifierErrorKind::MissingRegistry`. The test `dependency_rejects_org_repo_without_registry` at `dependency.rs:326-330` explicitly asserts this:
  ```rust
  let json = format!(r#"{{"identifier":"myorg/cmake:3.28@sha256:{}"}}"#, sha256_hex());
  let err = serde_json::from_str::<Dependency>(&json).unwrap_err();
  assert!(err.to_string().contains("explicit registry"));
  ```
  The example JSON in the docs would fail schema validation and runtime deserialization. The identifiers must carry an explicit registry, e.g. `ocx.sh/myorg/cmake@sha256:...` or `ghcr.io/myorg/cmake@sha256:...`.

  **This is a Block-severity error**: the example code snippet is invalid and will not parse.

### Missing nuance / drift [Warn]

- **"Basename" terminology: the source of the default name is the last path segment of the `repository` field, not the repository "basename" in the filesystem sense.**
  `Identifier::name()` at `identifier.rs:146-148` calls `rsplit('/').next()` on the `repository` field. For `ocx.sh/myorg/cmake`, the repository is `myorg/cmake` and `name()` returns `cmake`. For a single-segment repo like `ocx.sh/python`, repository is `python` and `name()` also returns `python`. The prose says "repository basename" which is broadly correct but slightly imprecise; "last path segment of the OCI repository" (as stated in the `Dependency::name()` doc comment at `dependency.rs:112`) is more precise.

- **The example identifiers would need registry prefixes like `ocx.sh/myorg/cmake` or `ghcr.io/myorg/cmake`.**
  If the intent is to show two organizations' `cmake` packages, the correct identifiers demonstrating the use case would be something like:
  - `"ocx.sh/myorg/cmake@sha256:..."` → default `name()` = `"cmake"` (basename of `myorg/cmake`)
  - `"ocx.sh/upstream/cmake@sha256:..."` → default `name()` = `"cmake"` (collision!)
  Using name overrides to disambiguate makes sense only when both produce the same basename, which only happens with multi-segment repositories (e.g., `myorg/cmake` and `upstream/cmake` as sub-paths under a common registry).

- **Prose claims "two dependencies share a basename (`myorg/cmake` and `upstream/cmake`)" — this only holds when `myorg` and `upstream` are sub-org paths, not registry names.**
  Under `Identifier::parse()`, `myorg/cmake` without a TLD would be treated as `ocx.sh/myorg/cmake` (via `parse_with_default_registry`) at the CLI level, but `Dependency` uses `Identifier::parse()` (strict, requires explicit registry). So the prose's examples without a registry prefix are doubly wrong: they would be rejected by the parser, and the collision scenario only works when the full identifiers are e.g. `reg.io/myorg/cmake` and `reg.io/upstream/cmake` (same basename `cmake` in the last segment of `myorg/cmake` and `upstream/cmake`).

- **`my-very-long-tool-name` as a motivating example for `name` override.**
  The prose says `name` helps when "the basename is awkward to use (`my-very-long-tool-name`)". The name override allows *renaming*, not shortening the regex-allowed form — the replacement must still satisfy `^[a-z0-9][a-z0-9_-]*$`. This is valid (a shorter alias is permitted), but the prose does not mention that the override value must itself satisfy the slug pattern, which is relevant context for the reader.

### Broken refs [Block]

- (none) — No internal anchor links in this section. The surrounding section at line 35 references `[reference-dependencies]` which resolves to `../reference/metadata.md#dependencies`, and that anchor exists in the reference doc.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **[Block] Both dependency `identifier` values in the example lack an explicit registry.**

  ```json
  { "identifier": "myorg/cmake@sha256:...",  "name": "myorg_cmake" }
  { "identifier": "upstream/cmake@sha256:...", "name": "cmake" }
  ```
  Neither `myorg` nor `upstream` contains `.` or `:` and neither is `localhost`, so both fail `has_explicit_registry()`. Runtime deserialization via `Identifier::parse()` → `PinnedIdentifier::try_from()` rejects both with "identifier must include an explicit registry". The example is non-functional as written.

  Corrected form (showing two packages from different orgs within the same registry):
  ```json
  { "identifier": "ocx.sh/myorg/cmake@sha256:...",    "name": "myorg_cmake" },
  { "identifier": "ocx.sh/upstream/cmake@sha256:...", "name": "cmake" }
  ```
  Or, from two different registries:
  ```json
  { "identifier": "ocx.sh/cmake@sha256:...",    "name": "ocx_cmake" },
  { "identifier": "ghcr.io/myorg/cmake@sha256:...", "name": "cmake" }
  ```

- **[Warn] `@sha256:...` ellipsis form is not self-evident about length requirements.**
  The digest must be a full 64-hex-character SHA-256 string (`Digest::Sha256` validated in `digest.rs`). The `...` placeholder does not communicate this constraint. Given the adjacent `#pinning` section uses the same convention and this is a doc example (not a schema definition), this is a Warn-level style issue, not a blocking error.

- **[Warn] The example's `env` entries use `"type": "constant"` without a `visibility` field.**
  The `visibility` field on `Var` defaults to `"private"` (see schema `v1.json:201-204`). `BUILD_TOOL` is labeled `"visibility": "public"` and `PATCH_SCRIPT` is labeled `"visibility": "private"`. The example is internally consistent; however the `Var` entries as written also do not include the required `"type"` field (`"type": "constant"`). Actually re-reading the verbatim text: they do include `"type": "constant"`. This is fine — no issue here.

### Style / convention violations [Warn]

- **"lookup key" is mild jargon** (`docs-style.md` Tooltip section: "Good candidates: technical terms, jargon, protocol-level concepts"). "lookup key" is borderline; readers unfamiliar with template interpolation may not immediately grasp it. A tooltip candidate: `<Tooltip term="lookup key">the `NAME` segment in `${deps.NAME.installPath}` used to match this dependency</Tooltip>`.

- **No `visibility` field is shown on the dependency entries in the example.** The `visibility` field is not mentioned in this section at all (it is covered in the next section `#edge-visibility`). The example implicitly uses the default `"sealed"` visibility. This is technically correct and reasonable for section focus, but omitting it creates a slight tension with the full-example shown in `reference/metadata.md` which always includes `visibility`.

---

## website/src/docs/authoring/dependencies.md#ordering — Ordering Matters

### Verified

- **"`constant`-type entries follow the last-wins rule"** — confirmed.
  `Env::apply_entries` (`crates/ocx_lib/src/env.rs:202`) dispatches `ModifierKind::Constant`
  to `self.set(...)`, which is a plain `HashMap::insert` overwrite. The last entry
  emitted for a given key wins. The in-depth doc (`in-depth/environments.md:108–109`)
  agrees and the worked example (`in-depth/environments.md:162–167`) demonstrates it.

- **"put dependencies whose env should 'win' closer to the end of the array"** — directionally
  correct for both constant and path entries. For sibling deps (neither is a transitive dep of
  the other), `ResolvedPackage::with_dependencies` (`crates/ocx_lib/src/package/resolved_package.rs:67–97`)
  processes the `deps` iterator in declaration order, so the last-declared sibling's entries are
  emitted later → its constants overwrite and its `path` entries prepend on top → it wins both.

- **`[in-depth-environments-last-wins]` anchor** — `../in-depth/environments.md#last-wins`
  exists at `website/src/docs/in-depth/environments.md:104`.

- **`[reference-deps-ordering]` anchor** — `../reference/metadata.md#dependencies-ordering`
  exists at `website/src/docs/reference/metadata.md:288`.

- **`path` entries prepend** — `Env::add_path` (`crates/ocx_lib/src/env.rs:137–151`) prepends:
  `new_value = entry_value + PATH_SEP + existing`. Each later-emitted path entry wins PATH lookup.
  Root is always emitted last in the `entries` vec (`composer.rs:182–204`), so root's `bin/`
  ends up first in PATH. The in-depth doc (`in-depth/environments.md:108`, worked example
  `in-depth/environments.md:146–152`) agrees.

### Inconsistent / hallucinated [Block]

- **(none)**

### Missing nuance / drift [Warn]

- **"The first entry's environment is applied first"** — true for the flat (no-nesting) case of
  sibling deps, but imprecise for the general case. Composition order is driven by the **TC in
  `resolve.json`** (built at install time, topological: deps before dependents), not directly by
  the raw `dependencies` array. When dep A (listed first) depends on dep B, B is emitted before
  A in the TC regardless of their declaration order. The statement is correct only for siblings;
  the section would benefit from a caveat: _"for direct (non-nested) deps, array order is
  preserved; transitive deps are always emitted before their dependents."_
  Sources: `crates/ocx_lib/src/package/resolved_package.rs:58–98` (TC construction),
  `crates/ocx_lib/src/package_manager/composer.rs:95–137` (composer TC iteration).

- **"`path`-type entries stack from later dependencies prepended onto earlier ones"** — the phrase
  "prepended onto earlier ones" is ambiguous. The actual mechanic is that each path entry is
  prepended in turn (`add_path` in `env.rs:137`); calling it "prepended onto earlier" could be
  read as "earlier in the array is prepended over later" (opposite direction). Better phrasing:
  "later deps' path entries prepend on top, so they appear first in PATH lookup." Low severity,
  but could mislead a publisher reasoning about priority direction.

- **Terse prose, no concrete worked example** — the `#ordering` section references the detail page
  but does not show even a two-dep `dependencies` array with the resulting PATH outcome. A minimal
  worked example (two sibling deps, which one wins) would let publishers internalize the
  "end-of-array wins" rule without needing to follow the link. Flag as Warn (Missing nuance).

- **"non-trivial graph" jargon** — opaque to publishers unfamiliar with graph terminology.
  Could be replaced with "multi-level dependency chain" or wrapped in a
  `<Tooltip term="non-trivial graph">a package that depends on packages that themselves have
  dependencies</Tooltip>`. Minor.

### Broken refs [Block]

- **(none)** — both link anchors verified present:
  - `in-depth/environments.md#last-wins` at line 104
  - `reference/metadata.md#dependencies-ordering` at line 288

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **No example in this section** — the section is prose-only and intentionally compact. The
  absence of an example is itself flagged as Warn under "Missing nuance" above. No invalid
  example exists.

### Style / convention violations [Warn]

- **"`[in-depth-environments-last-wins]`"** — the link label in the prose is rendered as its
  reference key text (`[in-depth-environments-last-wins]`) which is the anchor key, not a
  readable label. VitePress will render this as visible text if the bracket-with-key syntax
  is used directly in the prose instead of with a display label. Reviewing the actual
  markdown (`dependencies.md:77`), the link is `[last-wins rule][in-depth-environments-last-wins]`,
  which is correct reference-style with a human-readable display label. No violation.

- **Reference-style links** — all links in the section (`dependencies.md:77–79`) use the
  reference-style form (`[text][ref]`), with definitions collected at the bottom of the file
  (`dependencies.md:94–103`). Compliant with `docs-style.md`.

---

## website/src/docs/authoring/dependencies.md#pinning — Pinning by Digest

### Verified

- **"Tag advisory, digest mandatory"** — confirmed. `PinnedIdentifier` (`crates/ocx_lib/src/oci/pinned_identifier.rs:73`) rejects any `Identifier` without a digest at `TryFrom` time. Tag is `Option<String>` on `Identifier` (`crates/ocx_lib/src/oci/identifier.rs:32`). Test `dependency_without_tag` (`crates/ocx_lib/src/package/metadata/dependency.rs:354-359`) proves tag-less pinned identifiers parse successfully. Schema description at `website/src/public/schemas/metadata/v1.json:156` reads "optional advisory tag" — consistent with prose claim.

- **"Registry component required"** — confirmed. `Identifier::parse()` (`crates/ocx_lib/src/oci/identifier.rs:55-63`) calls `has_explicit_registry()` and returns `IdentifierErrorKind::MissingRegistry` if absent. Tests `dependency_rejects_bare_name` and `dependency_rejects_org_repo_without_registry` (`crates/ocx_lib/src/package/metadata/dependency.rs:320-331`) verify both `cmake:3.28@sha256:...` and `myorg/cmake:3.28@sha256:...` are rejected with "explicit registry" in the error message.

- **`identifier` field name** — confirmed. `Dependency` struct (`crates/ocx_lib/src/package/metadata/dependency.rs:93`) uses `pub identifier: oci::PinnedIdentifier`. Schema (`website/src/public/schemas/metadata/v1.json:59`) lists `"identifier"` as a required property on `Dependency`. Field is `required: ["identifier"]` in schema.

- **`visibility` field on dependency entries** — confirmed. Present in `Dependency` struct (line 99), `#[serde(default)]` so omission is valid. Schema shows `default: "sealed"` (`v1.json:76`). The JSON sample sets `"visibility": "public"` explicitly, which is a valid, non-default value.

- **`dependencies[].identifier` is a real schema field** — confirmed. The `Dependency` definition in `v1.json:56-84` lists `identifier` under `required` and references `#/$defs/PinnedIdentifier`.

- **`[oci-digest]` link definition** — confirmed. Line 89 of `dependencies.md` defines `[oci-digest]: https://github.com/opencontainers/image-spec/blob/main/descriptor.md#digests`.

- **`[reference-dependencies]` link target** — confirmed. Line 94 of `dependencies.md` defines `[reference-dependencies]: ../reference/metadata.md#dependencies`. Anchor `{#dependencies}` exists at `website/src/docs/reference/metadata.md:191`.

- **`#dependencies-no-version-ranges` anchor** — confirmed. Present at `website/src/docs/reference/metadata.md:302`: `### No Version Ranges {#dependencies-no-version-ranges}`. The text "no version ranges" in the pinning section is accurate as a reference to this decision.

- **Style** — prose-only introduction, JSON example, reference link well-placed. Conforms to `docs-style.md` reference-link convention (link definitions at bottom of file, reference-style links in body).

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **JSON sample truncated digest** — the example uses `"sha256:a1b2c3d4e5f6..."` (14 hex chars + ellipsis). The schema defines `PinnedIdentifier` as a plain `"type": "string"` with no `pattern` constraint (`v1.json:155-162`), so no schema-level validation failure occurs. However, the actual parser (`Identifier::parse` + `Digest` type) enforces a full 64-hex-char SHA-256; a real validator backed by the Rust deserializer would reject the truncated form. The ellipsis convention is common in documentation and is not a schema violation per the JSON Schema file, but readers who copy-paste the example literally and run it through the Rust parser (e.g., via `check-jsonschema` pointing at `v1.json`) would pass, while the actual OCX binary would reject it. Low practical risk (ellipsis is universal doc convention), but worth a note if the reference page ever adds live examples intended to be copy-pasted verbatim. The schema's own `examples` field at `v1.json:157-160` shows full 64-char digests, which is the better model.

- **JSON sample shows `"visibility": "public"`** — the default is `sealed` (confirmed in schema `v1.json:76` and `dependency_omitted_visibility_defaults_to_sealed` test at `dependency.rs:313-317`). The sample sets `"public"` explicitly. The pinning section's purpose is to illustrate digest pinning, not visibility — a sealed-default example (or omitting the field entirely) would be more representative of the common case. Not wrong, but could mislead readers into thinking `public` is the default or the standard choice. The `#edge-visibility` section correctly documents `sealed` as default. Flag as weak example choice.

- **`#name-field` section example** (`dependencies.md:44-45`) — two identifiers `myorg/cmake@sha256:...` and `upstream/cmake@sha256:...` lack an explicit registry component. These would be rejected by the `Identifier::parse()` strict parser used for `PinnedIdentifier` deserialization. This is outside the `#pinning` section under review, but directly adjacent and worth noting here: those bare org/repo identifiers are invalid and would fail. (Verify if this is the same file being fact-checked — it is `dependencies.md`, lines 44-45.)

### Broken refs [Block]

- (none) — all checked refs resolve.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **[Warn] Truncated digest in JSON sample** — `"sha256:a1b2c3d4e5f6..."` is not a valid SHA-256 digest (requires exactly 64 hex characters). The JSON Schema (`v1.json`) does not enforce a pattern on `PinnedIdentifier`, so schema validation via `check-jsonschema` will pass. The Rust parser will reject it at runtime. Standard documentation convention to use `...` truncation, but if the site ever adds a live validator or if readers copy-paste verbatim, this will fail. The schema's own `examples` array at `v1.json:157-160` uses full 64-char digests — the doc example should match that convention.

- **[Block] Invalid identifiers in adjacent `#name-field` example** (`dependencies.md:44-45`) — `"myorg/cmake@sha256:..."` and `"upstream/cmake@sha256:..."` lack an explicit registry. Both would be rejected by the `Identifier::parse()` path used for `PinnedIdentifier` deserialization, which enforces an explicit registry via `has_explicit_registry()`. Tests `dependency_rejects_org_repo_without_registry` (`dependency.rs:327-331`) confirm this. These identifiers are not in the `#pinning` section (lines 20-35) under review, but appear in the same file at the `#name-field` section and are factually incorrect — they will not parse. Flagged here because the fact-check covers the same file (`dependencies.md`) and these are compile-proven invalid.

### Style / convention violations [Warn]

- **`docs-style.md` link convention** — the `#pinning` section prose and the link defs follow reference-style links correctly. No inline `[text](url)` in body. Compliant.

- (no other violations in the `#pinning` section itself)

---

# website/src/docs/authoring/dependencies.md#when — When to Declare

Checked: 2026-05-07. Sources of truth consulted:
- `crates/ocx_lib/src/package/metadata/template.rs`
- `crates/ocx_lib/src/package/metadata/dependency.rs`
- `crates/ocx_lib/src/package/metadata/env/dep_context.rs`
- `crates/ocx_lib/src/utility/fs/assemble.rs`
- `crates/ocx_lib/src/file_structure/layer_store.rs`
- `crates/ocx_lib/src/file_structure/package_store.rs`
- `website/src/docs/reference/metadata.md`
- `website/src/docs/in-depth/storage.md`
- `mirrors/nodejs/mirror.yml`
- `.claude/rules/docs-style.md`
- nodejs.org download index (live fetch, v24.15.0)

---

### Verified

- **`${deps.cmake.installPath}` placeholder syntax** — correct. The token pattern `${deps.NAME.installPath}` is the only supported `deps.*` field. Confirmed in `template.rs:150` (`supported_fields: vec!["installPath"]`) and `dep_context.rs:90` (`"installPath" => Some(...)`). The docs at `reference/metadata.md:112–114` document this with identical syntax. No discrepancy.

- **"Consumer fetches it once"** — correct at the layer level. `assemble.rs:22–24` ("After assembly, every regular file in the destination shares an inode with its source in `layers/{digest}/content/`. This dedup is the whole point of the walker.") and `layer_store.rs` confirm the hardlink-from-shared-layer model. `storage.md:98–113` documents that a 200 MB shared base layer downloaded/extracted once is hardlinked into all packages that reference it. The claim is accurate: network fetch is once (layer dedup), disk bytes are once (hardlink dedup).

- **"Points at another package by digest"** — correct. `dependency.rs:88–107` shows `Dependency.identifier: oci::PinnedIdentifier` and `dependency_roundtrip` test (line 253) confirms `Visibility::SEALED` default and digest requirement. `reference/metadata.md:194–206` confirms digest-only resolution.

- **`nodejs:24` repository exists** — confirmed. `mirrors/nodejs/mirror.yml:1–4` registers `name: nodejs`, `target.registry: ocx.sh`, `target.repository: nodejs`. The short form `nodejs:24` matches OCX identifier conventions. The dependency example `nodejs:24` is accurate as an advisory tag reference (versions ≥ 20.0.0 per `mirror.yml:34`).

- **Linux `.tar.xz` format used for the nodejs mirror** — confirmed. `mirrors/nodejs/mirror.yml:13` registers `node-.*-linux-x64\\.tar\\.xz` as the linux/amd64 asset pattern.

- **Bundling claim conceptual accuracy** — correct. "Bundling means shipping the dependency's bytes inside your own archive" accurately contrasts with digest-pinned declaration.

---

### Inconsistent / hallucinated [Block]

- **"80 MB tarball" Node.js claim** — **WRONG size, wrong format.** The text at `dependencies.md:14` says "shipping the same 80 MB tarball." The linux/amd64 asset registered in `mirrors/nodejs/mirror.yml:13` is `node-.*-linux-x64.tar.xz`. The actual size of `node-v24.15.0-linux-x64.tar.xz` (live fetch from nodejs.org/download/release/latest-v24.x/) is **31 MB**, not 80 MB. The `.tar.gz` variant is 57 MB. Neither is 80 MB. The claim inflates the actual size by ~2.6× (xz) or ~1.4× (gz). Fix: cite 31 MB for `tar.xz` (the mirror-registered format) or ~57 MB for `tar.gz`, with version qualified (v24.x).

---

### Missing nuance / drift [Warn]

- **"Shares it across every package that references it"** — slightly imprecise framing. The doc says the consumer "fetches it once and shares it across every package that references it." This is true for layers (hardlink dedup), but the *package* store entry is keyed by digest only (not repo) — so two `npm-tool` wrappers both declaring `ocx.sh/nodejs:24@sha256:…` with the same digest share one package directory (no second download, no second copy). The sharing mechanism is hardlinks at the layer level; the "once" claim holds. However, the statement "consumer fetches it once" could mislead readers into thinking OCX forces a single global install per machine. In reality, OCX installs a dependency per unique digest regardless of how many top-level packages pin it — all those dependents hardlink to the same layer bytes. No bug, but the prose simplifies past the layer/package distinction documented in `storage.md:98–113`.

- **"Pointing at another package by digest"** — the text says "depends means pointing at another package by digest." The actual mechanism is a `PinnedIdentifier` that may reference an Image Index (multi-platform, resolved at install time) or a single manifest. `dependency.rs:83–87` ("The digest references either an OCI Image Index (for platform-aware resolution) or a single manifest"). The docs do not surface this distinction, which is relevant for multi-platform packages. Acceptable simplification for a "when to declare" intro, but the reference/metadata.md#dependencies-entry note covers it.

---

### Broken refs [Block]

- (none) — All internal links in the section (`reference/metadata.md`, `authoring/dependencies.md` anchors) exist and have content. No dangling anchors detected.

---

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **`nodejs:24` short-form identifier** — [Warn] The short form `nodejs:24` omits the required registry prefix (`ocx.sh/nodejs:24`). Per `dependency.rs:321–323` and `reference/metadata.md:298–300`, dependency identifiers without an explicit registry are rejected at deserialization ("no default registry fallback"). In a narrative "bundle vs. depend" example this is illustrative and not a schema sample, so it is tolerable in context, but the omission of `ocx.sh/` is technically inconsistent with the project's stated rule that "every dependency is a fully-qualified `registry/repo:tag@sha256:…` string, registry mandatory." The same section at line 14 says "A declared `nodejs:24` dependency" without qualification.

  Fix (Warn): change `nodejs:24` to `ocx.sh/nodejs:24` for consistency, or add a parenthetical noting the short form is illustrative and not valid in `metadata.json`.

---

### Style / convention violations [Warn]

- **External tools not hyperlinked** — `docs-style.md` §"Real-World Examples": "Every external tool mentioned must hyperlink — every occurrence, not just first. Never write 'Bazel rules' or 'devcontainer features' without link."

  Violations in the `#when` section (`dependencies.md:8–19`):

  | Tool | Occurrences | Link present? |
  |------|-------------|---------------|
  | Node.js | line 14 | No |
  | npm (implied via "npm-tool wrapper") | line 14 | No |
  | `terraform` | line 15 | No |
  | `cmake` | line 16 | No |

  `terraform` → https://www.terraform.io/ (or https://developer.hashicorp.com/terraform)
  `cmake` → https://cmake.org/
  `Node.js` → https://nodejs.org/

  Note: `cmake` does have a link defined at the bottom of the file (`[cmake-tll]`) but only for the CMake `target_link_libraries` docs page — not for cmake as a tool in the `#when` bullets. A tool-level link is missing.

- **`nodejs:24` vs `ocx.sh/nodejs:24`** — as noted under "Example issues" above. Also a style concern: it sets a different naming pattern from the pinning example at lines 27–34 which uses the full `ocx.sh/cmake:3.28@sha256:a1b2c3…` form.

- **"Vendored library" jargon** — `docs-style.md` recommends `<Tooltip>` for jargon. "Vendored library you patched" at line 18 contains two potentially unfamiliar terms ("vendored" and "patched" in context). Borderline: "vendored" is widely understood by the target audience (platform/infra engineers), so a tooltip is optional. Not a hard violation.

---

## Summary counts

| Severity | Count | Items |
|---|---|---|
| Block | 1 | "80 MB tarball" wrong size/format |
| Warn | 4 | Shared-once nuance; nodejs:24 missing registry; 4 unlinked external tools; vendored jargon (borderline) |

---

## website/src/docs/authoring/env-surface.md#last-wins — Last-Wins for Constants

Checked against: `crates/ocx_lib/src/package_manager/composer.rs`, `crates/ocx_lib/src/env.rs`, `crates/ocx_lib/src/package/resolved_package.rs`, `crates/ocx_lib/src/package_manager/tasks/profile_resolve.rs`, `crates/ocx_lib/src/package/metadata/env/conflict.rs`, `crates/ocx_lib/src/ci/github_flavor.rs`, `website/src/docs/in-depth/environments.md`, `website/src/docs/user-guide.md`, `.claude/rules/docs-style.md`.

---

### Verified

- **"last one in canonical dependency order wins"**: `env.rs:202` — `ModifierKind::Constant => self.set(&entry.key, &entry.value)` — a plain `HashMap::insert`; last write wins. Entries are applied sequentially by `apply_entries` in the order `composer::compose` emits them.
- **Topological order**: `resolved_package.rs:25-26` doc — "in topological order (deps before dependents)". `with_dependencies` at `resolved_package.rs:67-96` bubbles transitive deps before the direct dep, building depth-first topo order. `composer.rs:96` — "Each root's TC is already flat. Iterate in topological order (deps before dependents). Dep contributions emit before root's own contributions."
- **Root is always last**: `composer.rs:182-204` — root's own env vars are emitted after the full TC walk, so root's constants always overwrite any TC constant with the same key.
- **`#last-wins` anchor in `in-depth/environments.md`**: `in-depth/environments.md:104` — `## Last-Wins Scalar Semantics {#last-wins}`. Link target exists.
- **Link definition for `[in-depth-environments-last-wins]`**: `env-surface.md:119` — `[in-depth-environments-last-wins]: ../in-depth/environments.md#last-wins`. Resolves correctly.
- **Style: terse prose-only section**: Acceptable per docs-style.md.

---

### Inconsistent / hallucinated [Block]

- **"The diagnostic OCX surfaces when the conflict is meaningful"** — this claim in `env-surface.md:48` defers to the in-depth doc for the full rule, but the in-depth doc (`environments.md:109`) states: "OCX emits a warning to stderr only if two *unrelated* TC entries set the same constant." In the source, **no constant-overwrite warning exists in the main exec/env path**. `composer::compose` (`composer.rs:64-208`) contains no constant-conflict detection; `resolve_env` (`tasks/resolve.rs:203-209`) delegates directly to `composer::compose` with no post-processing for constant conflicts. Constant conflict detection only exists in two narrower paths:
  - `tasks/profile_resolve.rs:104-118` — `ocx shell profile load`
  - `ci/github_flavor.rs:91-92` — `ocx ci export` (GitHub flavor only)

  The phrase "the diagnostic OCX surfaces" implies a general capability in `ocx exec` / `ocx env`; that is not implemented. The in-depth doc's claim ("OCX emits a warning") is also inaccurate for those commands. Both pages overstate the diagnostic coverage.

  Cite: `composer.rs` (no constant-conflict warning); `env.rs:197-204` (apply_entries — no conflict check); `conflict.rs:31-65` (ConstantTracker exists but is only wired into profile and CI export flows).

---

### Missing nuance / drift [Warn]

- **"The first declaration is replaced silently"**: Accurate for the primary exec/env composition path (`composer.rs` + `apply_entries`). However, the in-depth doc (`environments.md:169-171`) claims a warning is emitted for unrelated TC entries — that claim is itself a divergence from the code (see Block above). The surface page's "silently" is therefore more accurate than the in-depth doc for exec/env, but creates inconsistency with the in-depth page it links to for "the full rule."
- **Conflict condition in in-depth doc**: `environments.md:169` says "no error is raised … wins silently" then `environments.md:109` says "a warning is emitted." These two statements in the same doc are contradictory; `environments.md:169-171` (the `::: warning` callout) is closer to code reality.
- **"seal the other's env"**: Assumes reader knows `sealed` visibility. A cross-reference to the visibility section (`#visibility` on this same page) or the in-depth doc would clarify. Not a correctness error.

---

### Broken refs [Block]

- **`[ug-conflicts]` link in `in-depth/environments.md:273`**: Points to `../user-guide.md#dependencies-environment`. This anchor does not exist in `user-guide.md`. The page has `{#dependencies}` (line 121) and no `{#dependencies-environment}` anchor. The "Conflict warnings" subsection (user-guide.md:159) has no explicit `{#…}` anchor — its auto-generated anchor is `#conflict-warnings`, not `#dependencies-environment`. The broken link is in the in-depth page (the link target referenced from env-surface.md); VitePress will silently produce a dead fragment on the final page. Cite: `in-depth/environments.md:273`, `user-guide.md` (no `#dependencies-environment` heading).

---

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none)

---

### Style / convention violations [Warn]

- **"two Java distributions" — missing hyperlink**: `docs-style.md` states "Every external tool mentioned must hyperlink — every occurrence." The word "Java" in "two Java distributions each declaring `JAVA_HOME`" names the Java platform/ecosystem without a hyperlink (e.g., to https://www.java.com/ or https://openjdk.org/). This is the only external tool reference in the section and it is unlinked. Cite: `env-surface.md:48`; docs-style.md "Real-World Examples and External Links."

---

## website/src/docs/authoring/env-surface.md#migrating — Migrating from Implicitly Public

### Verified

- **Default is `private`**: Confirmed in `crates/ocx_lib/src/package/metadata/visibility.rs:41` (`default_entry_visibility()` returns `Visibility::PRIVATE`) and `crates/ocx_lib/src/package/metadata/env/var.rs:128` (test `var_deserialize_absent_visibility_defaults_to_private`). Schema also documents `"default": "private"` on the `visibility` property of `Var` (`website/src/public/schemas/metadata/v1.json:202`).

- **`metadata.md` agrees on default**: `website/src/docs/reference/metadata.md:127,151` both state `Default: "private"` for `visibility` on path and constant entries.

- **"Vars without `visibility` field now default to `private` — they reach the package's own launchers but not consumers"**: Correct. `Visibility::PRIVATE = {private: true, interface: false}` means `has_private()=true` (self runtime visible) and `has_interface()=false` (not propagated to consumers). Confirmed in `visibility.rs:94-97`.

- **"`sealed` is rejected at parse time on `env` entries"**: Confirmed — `deserialize_entry_visibility` at `visibility.rs:22-33` rejects `"sealed"`; `EntryVisibility` schema enum only lists `["private", "public", "interface"]` (`v1.json:93-98`).

- **"implicitly public" historical claim**: Accurate. Before commit `795920cb` ("feat(package)!: package entry points"), `Var` had no `visibility` field (confirmed via `git show 795920cb^:crates/ocx_lib/src/package/metadata/env/var.rs`) and the old `Accumulator`/`Exporter` emitted all declared vars to all callers with no surface gate. Behavioral equivalent of all vars being public.

- **"Entry visibility arrived with the entry-points feature"**: Confirmed. Commit `795920cb` introduced both `entrypoints` on `Bundle` and `visibility` on `Var` simultaneously.

- **Both JSON samples are schema-valid**: Manually cross-checked against `website/src/public/schemas/metadata/v1.json`. `"before"` sample has all required fields (`type`, `version`; each env entry has `key`, `type`, `value`); `visibility` is optional (not in `required`). `"after"` sample adds `"visibility": "public"` which is valid per `EntryVisibility` enum. Both samples are valid against the current schema.

- **`ocx exec PKG -- cmd` syntax**: Correct. `crates/ocx_cli/src/command/exec.rs:41` defines `packages` with `value_terminator = "--"` and `command` as a separate positional after `--`. The `--` separator is load-bearing.

- **Synthetic `PATH ⊳ <pkg-root>/entrypoints` added at exec time**: Confirmed. `crates/ocx_lib/src/package_manager/composer.rs:558-568` shows `synth_entrypoints_path_for(pkg)` producing a `PATH`-type `Entry` pointing to `pkg.entrypoints()` which is `PackageDir.dir.join("entrypoints")` (`crates/ocx_lib/src/file_structure/package_store.rs:91-92`). This is called from `emit_dep_path_block` and `emit_root_path_block` during `compose()`, which runs at exec time, not install time.

- **"OCX adds this automatically at exec time"**: Correct. The synth-PATH is added by `composer::compose()` which is invoked by `resolve_env()` at exec time, not baked into any install artifact.

- **`"private"` means launcher-only**: Correct. When `visibility` is `private` on the `${installPath}/bin` PATH entry, it contributes to the private surface (`has_private()=true`) so the package's own generated launchers see it (via `--self` / `ocx launcher exec`), but it does not reach consumers (`has_interface()=false`).

- **Anchor IDs `{#migrating-diff}` and `{#migrating-decision}`**: Both present in the file at lines 60 and 92. Pattern `migrating-{subsection}` follows docs-style `{#parent-subsection}` convention.

- **JSON samples use correct field names and types**: `"type": "path"` / `"type": "constant"` match the schema's `oneOf` discriminator (`v1.json:168-193`). `"visibility": "public"` matches `EntryVisibility` enum.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **"entry-points feature port" phrasing** (line 58): The phrase "entry-points feature port" is ambiguous. The commit message uses `feat(package)!:` — this was a *new* feature (not ported from another system). The word "port" in English can be read as "portability rollout" but is nonstandard and potentially confusing. The metadata version remained `1` throughout (the visibility field was added as an optional addition to v1, not a new version). Recommend replacing "entry-points feature port" with "entry-points feature release" or just "this release."

- **"If your package has no declared entrypoints and relies entirely on consumers invoking `ocx exec PKG -- cmd`"** (line 90): `ocx exec` accepts multiple packages (`num_args = 1..`), so `PKG` is singular only as a simplified example. A more complete notation would be `ocx exec PKG... -- cmd`, though `PKG` as singular example is not wrong.

- **"entry-points feature" bundling claim** (line 58): The claim "One migration window, not two" implies separate migration events were possible. This is an internal product decision rather than something verifiable against source code. The `project_breaking_compat_next_version` memory confirms the breaking-compat direction (strict, no fallback). No block-tier issue, but the claim cannot be directly source-verified.

### Broken refs [Block]

- (none) — anchors `{#migrating}`, `{#migrating-diff}`, `{#migrating-decision}` are all present. The callout references `[authoring-entry-points]` which resolves to `./entry-points.md` (line 126). `entry-points.md` exists at `website/src/docs/authoring/entry-points.md`.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **`"before"` sample omits `$schema`**: The full example in `metadata.md:line 65` includes `"$schema"`. The before/after samples in the migration diff omit it. This is intentional (showing the minimal metadata before migration) and not a schema error since `$schema` is not a required field. Weak: [Warn] — could add a comment noting `$schema` is optional but the full example includes it.

- **`"after"` sample has trailing whitespace alignment**: Cosmetic only (alignment spaces in JSON are valid). Not an issue.

### Style / convention violations [Warn]

- **"Java" and "CMake" as referenced tools lack hyperlinks** (line 97, decision guide table): The table row `| \`JAVA_HOME\`, \`CMAKE_ROOT\`, tool prefix vars |` names Java and CMake implicitly via their env variable names. Line 48 in the same file (outside the `#migrating` section) mentions "Java distributions" without a hyperlink to Java. Per `docs-style.md` §"Real-World Examples and External Links": "Every external tool mentioned must hyperlink — every occurrence." `JAVA_HOME` and `CMAKE_ROOT` in a decision guide imply Java and CMake as tools — hyperlinks to https://www.java.com or https://openjdk.org (for Java) and https://cmake.org (for CMake) are required. [Warn]

- **Before/after code blocks use two separate fences with prose between** (lines 64-88): Per `subsystem-website.md`, VitePress `::: code-group` enables tabbed comparison for side-by-side alternatives. The current two-fence layout (before prose, then after prose) is functional but the tabbed code-group would improve scanability for the diff comparison. [Warn — style improvement, not a blocking issue]

- **`ACLOCAL_PATH` in decision guide** (line 98): Less common than `MANPATH`/`PKG_CONFIG_PATH`. No link to documentation for what it is. Minor — no strict rule requires linking env var names.

---

## website/src/docs/authoring/env-surface.md#templates — Templates and Dependency Paths

### Verified

- **Two placeholders — `${installPath}` and `${deps.NAME.installPath}` only.**
  `crates/ocx_lib/src/package/metadata/template.rs:19-32` (module-level doc) and `resolve_inner` (lines 83–170) handle exactly two token forms. No other substitutions exist. `validation.rs:219-229` (the W1 unknown-placeholder pass) confirms any other `${...}` is rejected.

- **`content/` is the install-target dir name.**
  `crates/ocx_lib/src/file_structure/package_store.rs:38-40` (`PackageDir::content()` returns `self.dir.join("content")`). `crates/ocx_lib/src/package_manager/composer.rs:147,194,269` pass `pkg.content()` as the `install_path` into `EnvResolver`. The doc's phrase "package's own `content/` directory" is correct.

- **`NAME` is the repository basename or the explicit `name` field.**
  `crates/ocx_lib/src/package/metadata/validation.rs:112-128` (`build_name_and_collision_maps`) confirms the map key is `dep.name()`, which is either the explicit `name` field or the repository basename — exactly as stated.

- **Only `installPath` is a supported dep field.**
  `crates/ocx_lib/src/package/metadata/env/dep_context.rs:88-93` (`resolve_field`): only `"installPath"` returns `Some`; all others return `None`. `template.rs:145-151` converts `None` → `TemplateError::UnknownDependencyField` with `supported_fields: vec!["installPath"]`.

- **`${deps.*}` references are validated at publish time.**
  `crates/ocx_lib/src/package/metadata/validation.rs:69-89` (`ValidMetadata::try_from`) calls `validate_env_tokens` before construction succeeds. Both `package_push.rs:84` and `package_create.rs:81` call `ValidMetadata::try_from(…)` before proceeding. The check happens on both paths.

- **Validation rejects undeclared dep names and unsupported fields.**
  `validation.rs:183-211` returns `TemplateError::UnknownDependencyRef` or `TemplateError::UnknownDependencyField` respectively.

### Inconsistent / hallucinated [Block]

- **"At install time" — imprecise for env vars; templates are resolved at exec time, not stored.**
  The doc says `${installPath}` "resolves to the absolute path … at install time." This conflates two different resolution phases:
  - For **env vars**: `TemplateResolver::resolve` is called inside `EnvResolver::resolve` → inside `composer.rs` → at `ocx exec` / `ocx env` time (exec time), not install time. The resolved string is never persisted; `template.rs:57-58` (`resolve` mode) is the *runtime* path.
  - For **entrypoint targets**: `TemplateResolver::resolve_for_validate` is used at publish time (syntax + reference check only), and then `TemplateResolver::resolve` is called again at launcher-generation time (`package_manager/launcher/generate.rs:64`), which is install time. But this is for entrypoints, not env vars.

  The doc's section is about env var `value` templates. The correct description is that env var templates are **resolved at exec time** (when `ocx exec` or `ocx env` is called). Values are never pre-resolved and stored. The phrase "at install time" is wrong for env vars.

  Source: `crates/ocx_lib/src/package/metadata/template.rs:26-31` (module doc distinguishes runtime mode vs publish-time mode); `crates/ocx_lib/src/package_manager/composer.rs:8-12` (compose called at exec time); `crates/ocx_lib/src/package/metadata/env/resolver.rs:50-63` (resolver called per-var at runtime).

### Missing nuance / drift [Warn]

- **"Typo gets caught before the manifest goes up" — ambiguous about `create` vs `push`.**
  The doc says validation happens "at publish time." Both `ocx package push` (`package_push.rs:84`) and `ocx package create --metadata` (`package_create.rs:81`) call `ValidMetadata::try_from`. The validation therefore fires at **create time** (local, no network) when `--metadata` is supplied, and again at **push time** (network step). "Before the manifest goes up" is accurate for `push` but understates that the same check fires locally during `create`. A publisher who uses `ocx package create` gets the error before any network interaction.

- **No JSON example showing a template value.**
  The section is prose-only. A concrete `metadata.json` snippet (`"value": "${installPath}/bin"` or `"value": "${deps.cmake.installPath}/bin/cmake-gen"`) would make the syntax immediately actionable. Acceptable per doc-style (prose sections can stand alone), but the example the docs gives narratively (`${deps.cmake.installPath}/bin/cmake-gen`) could be shown in a code block. Flagged as Warn: missing example.

- **`${deps.NAME.installPath}` resolves to the dep's `content/` dir, not just "content directory" generally.**
  The doc says "a declared dependency's content directory." Source: `dep_context.rs:65` doc comment reads "`packages/.../content/`" — it is specifically the `content/` subdirectory of the dep's package dir in the OCX object store. Saying "content directory" is technically accurate but omits that the filesystem path is `$OCX_HOME/packages/{registry}/{digest}/content/`. Not a block, but slightly imprecise for advanced readers.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **No code example present. [Warn]**
  The narrative example (`${deps.cmake.installPath}/bin/cmake-gen`) is embedded in prose only. No fenced code block or JSON snippet. The example is syntactically valid per source but has no rendered code sample. Flag: Warn (weak — example embedded in prose, not in a code block).

### Style / convention violations [Warn]

- **Technical terms `${installPath}` and `${deps.NAME.installPath}` appear inline without Tooltip.**
  Per `docs-style.md`: Tooltip is recommended for "technical terms, jargon, protocol-level concepts." These placeholders are novel syntax that authoring-audience users may encounter for the first time. Not using `<Tooltip>` is acceptable (the rule says "good candidates", not mandatory), but the `docs-style.md` guidance suggests they warrant it. Flag: Warn (style drift, not a block).

- **Section is prose-only with no code block or callout.**
  `docs-style.md` pattern: "Tables and code blocks follow prose; prose set context first." The section has prose setting context but never delivers a code block. Acceptable per strict reading, but weak. Warn-level.

---

## website/src/docs/authoring/env-surface.md#types — Path Variables vs Constants

### Verified

- **Two `type` values only — `path` and `constant`**: Confirmed. `Modifier` enum in
  `crates/ocx_lib/src/package/metadata/env/modifier.rs:13` has exactly two variants:
  `Path(path::Path)` and `Constant(constant::Constant)`. `ModifierKind` mirrors them.
  Serialized via `#[serde(tag = "type", rename_all = "snake_case")]`, producing `"type":
  "path"` and `"type": "constant"` on the wire. No third type exists.

- **`path` semantics — prepend (not append)**: Confirmed.
  `crates/ocx_lib/src/package/metadata/env/path.rs:8` doc-comment: "Path variables are
  prepended to any existing value of the environment variable." `Env::add_path()` at
  `crates/ocx_lib/src/env.rs:137-151` constructs `new_value = incoming + PATH_SEPARATOR +
  existing`, inserting the new segment before the existing value. Unit test
  `env_add_path_prepends` at `env.rs:396-404` asserts result starts with `/opt/bin` and
  ends with `/usr/bin`. Composer comment at `composer.rs:183-184` also confirms "root's
  PATH prepends win lookup over dep contributions (per `add_path` prepend semantics)."

- **`constant` semantics — replace**: Confirmed.
  `crates/ocx_lib/src/package/metadata/env/constant.rs:8` doc-comment: "Constant
  variables replace any existing value of the environment variable." `Env::set()` at
  `env.rs:133-135` is a plain `HashMap::insert()`, which overwrites any prior value.
  `apply_entries()` at `env.rs:197-205` dispatches `ModifierKind::Constant` to `set()`.
  Unit test `apply_entries_constant` at `env.rs:481-492` confirms the value is written
  unconditionally.

- **"Last one in dependency order wins" for constants**: Confirmed as accurate phrasing.
  Composer emits entries in topological order (deps before root; `composer.rs:95-204`).
  `apply_entries` iterates sequentially; each `Constant` entry calls `set()`, which is
  `HashMap::insert()`. The final insertion for any given key survives — the last emitted
  wins. Root's own env vars are always emitted after TC entries (`composer.rs:182-204`),
  making root always last. The in-depth page (`in-depth/environments.md:109`) terms this
  "last-writer-wins," which is a synonym from the runtime application perspective. Both
  phrasings are correct and consistent.

- **`path` entries stack from multiple packages**: Confirmed. `add_path()` always prepends
  onto any existing value, so successive calls accumulate entries without overwriting.
  Composer comment at `composer.rs:392-397` explains prepend semantics explicitly.

- **`[reference-env]` link target `../reference/metadata.md#env`**: Confirmed. Anchor
  `{#env}` exists at `website/src/docs/reference/metadata.md:103` ("## Environment
  Variables {#env}"). The section documents both `path` and `constant` shapes in detail.

- **`[in-depth-environments]` link target `../in-depth/environments.md`**: Confirmed.
  File `website/src/docs/in-depth/environments.md` exists.

- **`[in-depth-environments-last-wins]` link target `../in-depth/environments.md#last-wins`**:
  Confirmed. Anchor `{#last-wins}` exists at `website/src/docs/in-depth/environments.md:104`
  ("## Last-Wins Scalar Semantics {#last-wins}").

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **Terminology drift: "last one in dependency order" vs "last-writer-wins"**: The
  `#types` section says "last one in dependency order wins." The in-depth canonical page at
  `environments.md:109` calls the same semantics "last-writer-wins." The user-guide at
  `user-guide.md:161` uses "last-writer-wins." The `authoring/dependencies.md:77` page
  uses "last-wins rule." Minor drift in vocabulary between authoring and in-depth pages —
  not wrong, but inconsistent. The `#types` section phrasing is accurate for its context
  (dependency-declaration order) but a reader who moves to the in-depth page will
  encounter the different label. Low severity since both pages interlink; `Warn` because
  a consistent term across authoring and in-depth would help. The `#last-wins` anchor in
  `env-surface.md:46` uses its own phrasing "last one in canonical dependency order."

- **"PATH-like list" — tooltip candidate per docs-style.md**: `docs-style.md` recommends
  `<Tooltip>` for technical jargon that interrupts prose flow. "PATH-like list" is a
  reasonable term most publishers will know, but `docs-style.md:66` marks PATH-like
  lists as a good tooltip candidate. No structural defect; noted as style opportunity.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none) — the `#types` section has no code samples or JSON; it is prose-only. The
  referenced examples live in `metadata.md#env`, which is accurate (both `path` and
  `constant` JSON examples shown at `metadata.md:129-135` and `metadata.md:153-158`).

### Style / convention violations [Warn]

- **"Last-wins" appears for the first time in this section without a tooltip**: Per
  `docs-style.md`, technical terms with non-obvious meaning that interrupt prose flow are
  tooltip candidates. "last-wins" is used inline without a `<Tooltip>` on first
  occurrence. Given that `env-surface.md:46` has a dedicated `## Last-Wins for Constants
  {#last-wins}` section later in the same file, the first inline mention at line 10 could
  carry a tooltip (e.g., "the final package in topological order whose declaration
  overrides all earlier ones") or simply a link to the subsection — currently it only
  links to the in-depth page. Minor inconsistency with docs-style guidance; `Warn` not
  `Block`.

- **Section is short prose-only, no code example**: `docs-style.md` says "Tables and
  code blocks follow prose; prose set context first." The section correctly establishes
  prose context and then points to `metadata.md` for detailed shapes, which is good
  reference-pointer discipline. No violation; this style is intentional and appropriate
  for an introductory orientation section.

---

## website/src/docs/authoring/env-surface.md#visibility — Choosing Visibility

### Verified

- **Three env-entry visibility values (`private` / `public` / `interface`)**: confirmed. `entry_visibility_schema` in `visibility.rs:51–58` restricts the JSON Schema enum to exactly `["private", "public", "interface"]`. `deserialize_entry_visibility` at `visibility.rs:22–33` accepts all three. Tests in `visibility.rs:424–446` and `var.rs:125–171` confirm acceptance and `sealed` rejection.

- **`private` is the default**: confirmed. `default_entry_visibility()` at `visibility.rs:41–43` returns `Visibility::PRIVATE`. `Var.visibility` uses `#[serde(default = "default_entry_visibility")]` at `var.rs:40`. Test `var_deserialize_absent_visibility_defaults_to_private` at `var.rs:128–133` pins this.

- **`sealed` rejected at parse time on `env` entries**: confirmed. `deserialize_entry_visibility` at `visibility.rs:27–30` returns an error with the message `"sealed is not a valid entry-level visibility; use private, public, or interface"` when `sealed` is encountered. This is distinct from dependency-edge visibility, where `sealed` is valid and is the `Default` (`Visibility::default()` = `SEALED` at `visibility.rs:87–90`; `Dependency.visibility` uses `#[serde(default)]` at `dependency.rs:98`).

- **Two-surface model**: confirmed. `Visibility` is a struct of two booleans `{ private: bool, interface: bool }` at `visibility.rs:77–82`. `has_interface()` and `has_private()` accessors at `visibility.rs:118–129` drive surface-gating in `composer.rs`. `emit_root_vars` at `composer.rs:354–384` gates on `has_interface()` (consumer view, `--self` off) vs `has_private()` (self view, `--self` on).

- **Table — `private` (No interface, Yes private)**: confirmed. `Visibility::PRIVATE = { private: true, interface: false }` at `visibility.rs:94–97`. `has_interface() = false`, `has_private() = true`.

- **Table — `public` (Yes interface, Yes private)**: confirmed. `Visibility::PUBLIC = { private: true, interface: true }` at `visibility.rs:99–104`.

- **Table — `interface` (Yes interface, No private)**: confirmed. `Visibility::INTERFACE = { private: false, interface: true }` at `visibility.rs:108–111`.

- **`interface` = "forwarded to consumers but not used by the package's own runtime"**: confirmed. `INTERFACE.private = false` means `has_private() = false`. `emit_root_vars` with `self_view=true` (launcher's private surface) gates on `has_private()`, so `interface` vars are excluded. Consistent with `visibility.rs:106`: "Env propagated to consumers but not used by the package itself." Generated launchers always use self-view (`--self`) internally (`subsystem-cli-commands.md`), so `interface` vars never reach the package's own launchers.

- **`[cmd-exec]` link definition resolves to `../reference/command-line.md#exec`**: confirmed. Defined at `env-surface.md:123`. Anchor `{#exec}` exists at `command-line.md:313`.

- **`[in-depth-entry-points]` link definition resolves to `../in-depth/entry-points.md`**: confirmed. Defined at `env-surface.md:120`. File exists at `website/src/docs/in-depth/entry-points.md`.

- **`[reference-deps-visibility]` link resolves to `../reference/metadata.md#dependencies-visibility`**: confirmed. Defined at `env-surface.md:114`. Anchor `{#dependencies-visibility}` exists at `metadata.md:229`.

- **`[in-depth-environments-two-surfaces]` link resolves to `../in-depth/environments.md#two-surfaces`**: confirmed. Defined at `env-surface.md:118`. Anchor `## Two Surfaces {#two-surfaces}` exists at `environments.md:8`.

- **`:::tip` callout usage**: correct. The tip contains actionable authoring advice ("Mark them all `"visibility": "public"`"), matching `docs-style.md` callout type `tip = actionable advice, example usage, recommended patterns`.

- **Table structure (three values, two surfaces, use case column)**: correct. Well-formed with `| Value | Interface surface | Private surface | Use case |` headers mapping all three valid values.

- **`sealed` rejected clarification in prose ("a declared entry that contributes to neither surface is dead configuration")**: consistent with source. `visibility.rs:15` doc comment: "a `Var` invisible everywhere (neither self nor consumer) is dead config — see ADR Tension 4."

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **`ocx exec mypkg` syntax is incomplete**: `env-surface.md:29` uses the inline backtick text `` `ocx exec mypkg` `` as if it is a standalone invocable command. The actual required syntax is `ocx exec <PACKAGES>... -- <COMMAND> [ARGS...]` (`command-line.md:328`); both `<PACKAGES>` and `<COMMAND>` are `required = true` (`exec.rs:41,51`). Running `ocx exec mypkg` without `-- <cmd>` would be rejected by clap at argument parsing. The text is clearly illustrative (not a code block), but the inline-code formatting implies a runnable form and could mislead authors about exec's interface. The `in-depth/environments.md:15` uses the more accurate `ocx exec PKG -- cmd` pattern consistently. Severity: **Warn** (misleading syntax representation, not a claim about behavior).

- **Interface surface description says "Public-by-default `PATH` entries"**: `env-surface.md:29` uses the phrase "Public-by-default `PATH` entries" in the interface surface bullet. The actual default for `Var.visibility` is `private`, not `public` (`visibility.rs:41`, `var.rs:128–133`). Authors must explicitly set `"visibility": "public"` or `"interface"` for a `PATH` entry to reach consumers. The phrase "Public-by-default" in this context reads as if PATH entries default to public, which contradicts the confirmed `private`-default rule. The surrounding context (`#migrating` section explains the migration) helps but the ambiguity in the interface-surface description bullet is a drift risk for authors reading only this section. Severity: **Warn**.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none — no JSON samples or cast files in the target subsection lines 25–44)

### Style / convention violations [Warn]

- **`cmake`, `node`, `uv` mentioned without hyperlinks** (`env-surface.md:43`): the tip reads "For a typical bare-binary package (cmake, node, uv)..." All three are named external tools. `docs-style.md` requires "Every external tool mentioned must hyperlink — every occurrence, not just first." Catalog pages exist for all three: `website/src/docs/catalog/cmake.md`, `website/src/docs/catalog/nodejs.md`, `website/src/docs/catalog/uv.md`. Each name should be a reference-style link to its catalog page. Three violations at one location. Severity: **Warn**.

---

## website/src/docs/authoring/entry-points.md#naming — Naming and Collisions

### Verified

- **Regex `^[a-z0-9][a-z0-9_-]*$`** — exact match. Source:
  - `crates/ocx_lib/src/package/metadata/slug.rs:11`: `pub const SLUG_PATTERN_STR: &str = r"^[a-z0-9][a-z0-9_-]*$";`
  - `crates/ocx_lib/src/package/metadata/entrypoint.rs:89` (JSON Schema description): `"must match ^[a-z0-9][a-z0-9_-]*$ and be at most 64 characters"`
  - `crates/ocx_lib/src/package/metadata/entrypoint.rs:179` (error message): `#[error("invalid entrypoint name '{name}': must match ^[a-z0-9][a-z0-9_-]*$")]`

- **64 character limit** — exact match. Source:
  - `crates/ocx_lib/src/package/metadata/slug.rs:12`: `pub const SLUG_MAX_LEN: usize = 64;`
  - `crates/ocx_lib/src/package/metadata/entrypoint.rs:24–28`: `MAX_LEN` mirrors `SLUG_MAX_LEN` with rationale (Windows `MAX_PATH = 260`).
  - Tests at `entrypoint.rs:261–300` confirm 64-char accepted, 65-char rejected.

- **"Names starting with `_` are rejected"** — consistent with regex. `[a-z0-9]` as first char excludes `_`. Test at `entrypoint.rs:229–232` (`name_rejects_leading_underscore`) confirms.

- **`__internal-helper` as rejected example** — starts with `_`, fails `[a-z0-9]` first-char class. Consistent.

- **`cmake-gen` as accepted example** — passes `^[a-z0-9][a-z0-9_-]*$`. Consistent.

- **`[select-collision]` link definition resolves** — `../reference/command-line.md#select-entry-point-collision` — anchor confirmed at `command-line.md:511`: `#### Entry-point name collisions {#select-entry-point-collision}`.

- **`[catalog]` link** (`../catalog.md`) — file exists at `website/src/docs/catalog.md`.

- **`[in-depth-entry-points]` link** (`../in-depth/entry-points.md`) — file exists at `website/src/docs/in-depth/entry-points.md`.

- **Bullets carry bolded leads** — style compliant with `docs-style.md`.

- **Section opens with idea/problem/solution flow** — compliant.

### Inconsistent / hallucinated [Block]

- **"OCX detects the collision at select time"** — **WRONG**. Collision detection happens at two earlier stages, not at `ocx select`:

  1. **Install time** (`crates/ocx_lib/src/package_manager/tasks/pull.rs:425`): `composer::check_entrypoints(std::slice::from_ref(&root_info), &fs.packages).await?;` — runs during `ocx install` (pull phase), before the atomic temp→object-store move.
  2. **Compose time** (`crates/ocx_lib/src/package_manager/composer.rs:64–72`): multi-root collision gate runs inside `compose()` whenever `ocx env` or `ocx exec` is given 2+ roots.

  The command-line reference itself contradicts the claim: `command-line.md:513` states "Entry point name collisions are checked at two distinct points; `select` itself performs no collision check, since flipping `current` does not compose environments." The `[select-collision]` anchor the doc links to is about the `select` command's documentation section, but that section explicitly says `select` has **no** entry-point collision check. The authoring page sends users to a reference section that refutes the claim in the authoring page.

  The correct phrasing is: OCX detects collision at **install time** (single-root: within the interface surface of the package being installed) and at **compose time** (multi-root: when `ocx env` or `ocx exec` is given two or more roots). The `[select-collision]` link target documents those two real gates.

- **"multi-owner error"** — this phrase does **not** appear as an error type, variant name, or user-visible message in any Rust source. The actual error is `PackageErrorKind::EntrypointCollision` (see `error.rs:132–135`). The in-depth page uses "Multi-Owner Collision Reporting" (`in-depth/entry-points.md:140`) as a section heading, but this is a doc-invented label, not a source-code term. The authoring page calls it a "multi-owner error" in a way that implies it is a named error format — this is at best imprecise, at worst misleading for publishers debugging errors.

### Missing nuance / drift [Warn]

- **Only "select time" collision mentioned** — the authoring summary omits the more commonly hit gate: install-time collision within a single package's interface surface. A publisher who ships two packages that transitively share a dep with conflicting entrypoints will hit the install gate, not a select gate. Pointing to `[select-collision]` (which is primarily about the select command's documentation, even though that section explains both gates) obscures the more common failure mode.

- **"two installed packages declare entrypoints with the same name"** — the scenario described (two independently installed packages with the same entrypoint name) is the multi-root compose-time case. The install-time case (a single package whose own TC has a conflict on the interface surface) is a different trigger entirely. The text conflates both into one scenario.

### Broken refs [Block]

- **`[select-collision]` semantic mismatch** — the link target (`command-line.md#select-entry-point-collision`) resolves correctly (anchor exists, file exists), but the surrounding claim ("OCX detects the collision at select time") contradicts what that section says. The section explicitly states `select` performs no collision check. This creates a broken semantic expectation: the link destination actively contradicts the claim it is supposed to support.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- `cmake-gen` — valid per regex. No issue.
- `__internal-helper` — correctly rejected (starts with `_`). No issue.
- `cmake`, `ctest`, `cpack` in bullet — all pass `^[a-z0-9][a-z0-9_-]*$`. No issue.

### Style / convention violations [Warn]

- **CMake mentioned without a hyperlink** (`cmake-gen`, `cmake`, `ctest`, `cpack` in bullets) — `docs-style.md` line 50: "Every external tool mentioned must hyperlink — every occurrence, not just first." CMake is referenced in the namespace guidance bullet ("If you ship CMake") without an outbound link to cmake.org or the upstream project. Flag per docs-style rule.

---

## website/src/docs/authoring/entry-points.md#python-example — Worked Example: Python Script with a Pinned Interpreter

### Verified

- **`ocx.sh/cpython:3.13` repository exists.** `mirrors/cpython/base.yml:1-3` sets `target.registry: ocx.sh` and `target.repository: cpython`. `mirrors/cpython/mirror-3.13.yml:1` extends `base.yml` with name `cpython-3.13`. The identifier `ocx.sh/cpython:3.13` in the JSON sample is correct.

- **`ocx launcher exec` subcommand exists and name is correct.** `crates/ocx_cli/src/command/launcher/mod.rs:26-35` defines the `Launcher` enum with a single `Exec` variant. `crates/ocx_cli/src/command/launcher/exec.rs:4` documents it as `` `ocx launcher exec '<pkg-root>' -- "$(basename "$0")" "$@"` ``. `crates/ocx_lib/src/package_manager/launcher/body.rs:56` shows the generated script literal `launcher exec '<pkg_root>'`.

- **JSON sample is schema-valid.** `website/src/public/schemas/metadata/v1.json` confirms:
  - `type: "bundle"` — required discriminant field (line 241)
  - `version: 1` — integer enum value 1 (line 213-217)
  - `dependencies[].identifier` — required `PinnedIdentifier` string (line 80-83)
  - `dependencies[].name` — optional `DependencyName` string matching `^[a-z0-9][a-z0-9_-]*$` (line 85-89)
  - `dependencies[].visibility` — optional `Visibility` enum `["sealed","private","public","interface"]` (line 218-225). Value `"private"` is valid.
  - `env[].key` — required string (line 199-204)
  - `env[].type: "path"` — required discriminant for `Path` variant (line 176)
  - `env[].value` — required string for path entries (line 145-148)
  - `env[].visibility` — optional `EntryVisibility` enum `["private","public","interface"]` (line 200-204). Value `"private"` is valid.
  - `entrypoints[].name` — required (line 103-105)
  - `entrypoints[].target` — required template string (line 107-109)
  - All fields present and typed correctly. The `"sha256:abc..."` in `identifier` is advisory and accepted by the `PinnedIdentifier` string pattern.

- **ENV composition order: "PATH from the cpython dep prepended, this package's `bin/` prepended on top" is correct.** `crates/ocx_lib/src/package_manager/composer.rs:97-98` states "Dep contributions emit before root's own contributions per ADR Algorithm v3." Lines 182-184: "Root's own contributions... Emit AFTER the TC so root's PATH prepends win lookup over dep contributions (per `add_path` prepend semantics)." Since `add_path` prepends (last pushed = first in PATH), the root's `bin/` is highest priority. The dep's PATH (cpython) is prepended first, then the root's `bin/` is prepended on top of that.

- **`.sh` and `.cmd` launchers generated.** `crates/ocx_lib/src/package_manager/launcher/body.rs:37` exports `unix_launcher_body` (Unix `.sh`). Line 82 exports `windows_launcher_body` (Windows `.cmd`). `website/src/public/schemas/metadata/v1.json:12` schema doc says "Each entry produces a Unix `.sh` script and a Windows `.cmd` batch file."

- **`ocx launcher exec` step description.** Step 2 in the doc says "The launcher re-enters via `ocx launcher exec` with the package root baked in." This matches `body.rs:56`: the generated Unix launcher body calls `launcher exec '<pkg_root>'`. The description is accurate.

- **`:::tip Native binaries don't need any of this` callout** — usage is fine. Factually accurate per the broader docs in `website/src/docs/authoring/entry-points.md:37`.

- **Second `:::tip Multi-platform launchers` — content is accurate.** Both `.sh` and `.cmd` are generated for every platform at install time (`body.rs`, schema doc). The `[in-depth-entry-points]` link in the tip resolves to `../in-depth/entry-points.md` which exists (`website/src/docs/in-depth/entry-points.md`).

---

### Inconsistent / hallucinated [Block]

- **`python:3.13` identifier inconsistency (Block).** In the last paragraph of the section, the doc refers to "bare `python:3.13`" — implying `ocx.sh/python:3.13` under the default registry. There is no `python` mirror; the only CPython mirror is `ocx.sh/cpython` (`mirrors/cpython/base.yml:1-3`, `website/src/docs/catalog/cpython.md:36`: "Use `cpython:3.13`"). The identifier should be `cpython:3.13` (resolving to `ocx.sh/cpython:3.13`), not `python:3.13`. As written, `python:3.13` would imply `ocx.sh/python:3.13`, which does not exist as a recognized OCX mirror. This is an internal inconsistency with the JSON sample directly above (which correctly uses `ocx.sh/cpython:3.13`).

  Affected text: `"that's what bare `python:3.13` is for"` — should read `"that's what bare `cpython:3.13` is for"`.

---

### Missing nuance / drift [Warn]

- **PATHEXT caveat misattributed to `in-depth/entry-points.md` (Warn).** The second `:::tip` says "The cross-platform caveats (PATHEXT on Windows, Git Bash quirks, PowerShell quoting) live in the [entry points in depth][in-depth-entry-points] page." `website/src/docs/in-depth/entry-points.md` covers Git Bash (`#git-bash`, line 190) and PowerShell (`#powershell`, line 174) but does NOT contain the word "PATHEXT" at all (confirmed by grep). PATHEXT caveats live in `website/src/docs/reference/command-line.md` (lines 309-310, 475-476, etc.) and `website/src/docs/faq.md` (lines 132-133). The tip misleads readers to look for PATHEXT guidance in `in-depth/entry-points.md` where it is absent. Either the caveat list in the tip should drop PATHEXT, or the tip should additionally link `command-line.md`.

---

### Broken refs [Block]

- **(none)**

---

### Example/JSON sample issues [Block if invalid, Warn if weak]

- **`"sha256:abc..."` is not a syntactically valid digest (Warn).** The `PinnedIdentifier` schema (`v1.json:156-161`) is a plain `type: string` with no pattern constraint, so `"ocx.sh/cpython:3.13@sha256:abc..."` passes schema validation. However, it is not a real digest and the truncation placeholder `sha256:abc...` could mislead readers about the required format (a full SHA-256 hex string). The example in the schema itself uses real 64-char digests (lines 157-160). This is a documentation-quality concern — weak example, not schema-invalid. Category: Warn.

- **`visibility` field on `env` entry uses `EntryVisibility` not `Visibility` (Verify OK).** The dep uses `"visibility": "private"` which is `Visibility` (dep axis). The env entry uses `"visibility": "private"` which is `EntryVisibility` (entry axis). Both are valid: `Visibility` enum includes `"private"` (`v1.json:219-225`), `EntryVisibility` enum includes `"private"` (`v1.json:93-98`). Correct fields on correct objects.

---

### Style / convention violations [Warn]

- **Anchor `{#python-example}` does not follow parent-subsection nesting pattern (Warn).** `docs-style.md:40` says "pattern `{#parent-subsection}` for nesting." This H3 is nested under `## Target Templates {#target}`. By convention the anchor should be `{#target-python-example}`. Existing H3 anchors in `in-depth/entry-points.md` (`#unix`, `#windows`, `#powershell`, `#git-bash`, `#cmd-empty-args`) also deviate from strict nesting, so this is a style consistency issue across the docs, not unique to this section. Flagging as Warn per the stated rule.

- **Second `:::tip` is more `:::info` than `:::tip` (Warn).** `docs-style.md` callout table: `:::tip` = "Actionable advice, example usage, recommended patterns"; `:::info` = "Analogies to other systems, background context." The "Multi-platform launchers" tip provides background context ("OCX generates `.sh` launchers... from the same metadata") and a cross-reference, not an actionable recommendation for the publisher. It fits the `:::info` pattern better. Borderline — not a block, but worth flagging.

- **External tools mentioned without hyperlinks (Warn).** `docs-style.md:47`: "Every external tool mentioned must hyperlink." In the target section the following appear unlinked:
  - `mise` (line 95 of the doc file) — mentioned as "`a `mise`-managed Python`" with no link.
  - `Go` (in the `:::tip` "statically-linked Go or Rust binary") — no link to golang.org.
  - `Rust` (in the same tip) — no link to rust-lang.org.
  - `Python` / `CPython` / `python3` — not linked (though `python-entry-points` link covers the packaging spec, not the language itself).
  - `PATHEXT`, `PowerShell`, `Git Bash` — mentioned in `:::tip` "Multi-platform launchers" without links; `in-depth/entry-points.md` does link `PowerShell` and `Git Bash` at their respective sections.

  Priority: `mise` is the clearest violation (directly mentioned by name in prose without link).

---

## website/src/docs/authoring/entry-points.md#target — Target Templates

### Verified

- **`target` field name** — confirmed. `Entrypoint` struct at `crates/ocx_lib/src/package/metadata/entrypoint.rs:110` declares `pub target: String`. Field name is exact.

- **`${installPath}` placeholder** — confirmed valid in `target`. `template.rs:83-99` shows `${installPath}` substitution in `resolve_inner`. Test at `entrypoint.rs:346` serde-round-trips `"${installPath}/bin/cmake"` as a `target` value.

- **`${deps.NAME.installPath}` placeholder** — confirmed valid in `target`. `template.rs:109-166` handles all `${deps.NAME.FIELD}` tokens. `validation.rs:412` calls `check_target_resolves` which invokes `resolve_for_validate`, confirming dep-path tokens are processed in the `target` field.

- **JSON sample 1 schema-validity** — confirmed. `{"name": "cmake", "target": "${installPath}/bin/cmake"}` is the exact example used in `entrypoint.rs:346` serde round-trip test.

- **JSON sample 2 — `visibility: "public"` on dependency** — confirmed valid. `dependency.rs:261-272` tests `"visibility":"public"` round-trip. `Dependency` struct at `dependency.rs:88-107` accepts `visibility` as `Visibility` (defaults to `SEALED`). `"public"` is a valid wire value.

- **JSON sample 2 — `${deps.cmake.installPath}` in entrypoint target** — confirmed valid. `validation.rs:645-657` test `entrypoint_valid_dep_install_path_target` accepts exactly this pattern (dep declared, target uses `${deps.foo.installPath}/bin/foocmd`).

- **"OCX rejects launchers whose target is missing" (when dep not installed)** — confirmed at runtime. `template.rs:159-163` returns `TemplateError::DependencyNotInstalled` when `ctx.install_path().exists()` is false. `generate.rs:84-87` wraps this in `crate::Error::EntrypointInstallFailed`, aborting before any launcher is written.

- **"`target` must exist at install time"** — partially confirmed with important nuance (see Warn section). The *dep existence check* (`.exists()`) happens at install-time resolution in `generate.rs:82-87`. However the target file itself (e.g. `bin/cmake`) is NOT separately probed for existence at install time — only that the dep root is present. See Missing Nuance below.

- **`${deps.NAME.installPath}` — `NAME` is basename or explicit `name` field** — confirmed. `dependency.rs:114-123` shows `dep.name()` returns explicit `name` if set, else repository basename. The docs description at line 58 matches exactly.

- **Python entry-points link target** — `https://packaging.python.org/en/latest/specifications/entry-points/` — this URL is used at `website/src/docs/reference/metadata.md:458` in the same project and matches the `[python-entry-points]` reference link definition. URL format is current Python Packaging Authority spec URL. Verified as correct.

- **Reference page cross-check** — `website/src/docs/reference/metadata.md` at anchor `#entry-points-fields` (line 327) lists `target` with description "Supports the same placeholders as `env` values" — consistent with the authoring page claim.

### Inconsistent / hallucinated [Block]

- **"The path must exist at install time — OCX rejects launchers whose target is missing."** — This statement is inaccurate in a material way. The install-time check in `generate.rs:82-87` resolves the `target` template via `TemplateResolver::resolve` to verify the template *parses and all dep references exist* (`DependencyNotInstalled` if a dep root is absent). The resolved target path (e.g. `/home/user/.ocx/packages/.../content/bin/cmake`) is **discarded** — `generate.rs:81` comment says "the result is discarded." The binary at that resolved path is never stat'd. The launcher bakes only the package root, not the resolved target. The `body.rs:22` doc comment states explicitly: "the launcher does NOT bake the resolved `target` — `target` is a publish-time existence assertion only." Resolution (including dep-root existence check) happens at install time, but the actual *binary file* existence is checked at exec time (when `ocx launcher exec` is called). The claim "OCX rejects launchers whose target is missing" is misleading: OCX rejects launchers whose dep *root* is missing (not the target binary). If `bin/cmake` simply does not exist in the content tree, install succeeds and the error surfaces only when the user invokes the launcher. **Source**: `generate.rs:78-87`, `body.rs:22`, `body.rs:53-57`.

### Missing nuance / drift [Warn]

- **"resolves at install time" vs "exec time"** — the doc's intro says "`target` field is the path OCX resolves at install time." This is imprecise in two ways: (1) `target` placeholder substitution happens at *both* install time (as a parse/reference check) and exec time (as the real resolution via `ocx launcher exec` reading `metadata.json`). The actual path used to find the binary is resolved at **exec time** — `body.rs:22` documents this: "reads `metadata.json` from that root and resolves the target via env interpolation at invocation time." The install-time resolution is only a validation probe whose result is discarded. The doc's mental model ("the path OCX resolves at install time") understates that real binary execution depends on exec-time resolution. This affects user understanding of when failures surface. **Sources**: `generate.rs:79-81` ("defense-in-depth check…result is discarded"), `body.rs:22-24`.

- **No other `${...}` placeholders exist** — the docs say "supports the same two placeholders as `env` values" which implies exactly two. The source confirms only `${installPath}` and `${deps.NAME.installPath}` are supported (any other `${...}` token is rejected as `UnknownPlaceholder` per `validation.rs:313-329`). Accurate, but the `template.rs:230-238` `UnknownPlaceholder` variant error message is worth knowing — unrecognized placeholders are a hard error at publish time, not silently ignored. The docs don't mention `${OCX_HOME}` or any others (correctly absent). No correction needed, flag only for awareness.

- **JSON sample 2 — `"visibility": "public"` as example choice** — structurally valid, but semantically unusual for a meta-package pattern. The typical meta-package that re-exposes a dependency's tool would use `"visibility": "interface"` (consumer-axis only) or `"sealed"` (content accessed by path in `target`, no env needed). Using `"public"` means the cmake dep's env (e.g. CMAKE_HOME if set) is also emitted on the private surface, which is not what most meta-packages need. This is a pedagogical Warn, not a Block — `"public"` is schema-valid. **Source**: `visibility.rs` constants; `reference/metadata.md:239-244` visibility table.

- **`target` field type is `String` (not path-typed)** — the Rust struct stores `target` as `pub target: String` (`entrypoint.rs:110`), not a `PathBuf`. This is a forward-slash-canonical template string, not a native OS path. The ADR (referenced in `validation.rs:820-828`) documents that `target` uses forward-slash separators on all platforms. The docs do not mention this constraint; Windows publishers writing `${installPath}\bin\cmake` with backslashes: `validation.rs:820-856` shows backslash-separated paths normalize but Windows-style traversals with backslash `..` are still rejected. Minor doc gap for cross-platform publishing guidance.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **JSON sample 2 — truncated digest `@sha256:abc...`** — `"ocx.sh/cmake:3.28@sha256:abc..."` will fail deserialization. `PinnedIdentifier` requires a full 64-hex-character SHA-256 digest; `dependency.rs:349-352` test shows a bare `"ocx.sh/java:21"` (no digest) is rejected with "digest" in the error. The `abc...` form is illustrative notation common in docs but is not a valid OCX identifier. This is a documentation pattern convention (not unique to this file — `reference/metadata.md:92` uses `a1b2c3d4e5f6...` identically) so it is a consistent convention, but technically the sample is not copy-pasteable as shown. **Warn** — consistent with project-wide pattern, but the doc should note that full digests are required or use a placeholder comment.

- **JSON sample 1 is schema-valid and copy-pasteable** — the `entrypoints` array with only `name` + `target` is correct; `bundle.rs` (via `Entrypoints` `#[serde(default)]`) makes the top-level `entrypoints` array optional. An isolated `{ "entrypoints": [...] }` fragment without the required `type` and `version` fields is an incomplete snippet, but this is standard doc practice and consistent with other code samples in the file. No issue.

### Style / convention violations [Warn]

- **"The path must exist at install time — OCX rejects launchers whose target is missing."** — Given the nuance described in Inconsistent above, this sentence reads as a `:::warning` callout candidate, but the claim itself needs to be corrected before it can be promoted to a callout. The docs-style rule at `.claude/rules/docs-style.md` lists `:::warning` for "Important caveats, commonly misunderstood things." If corrected to accurately describe what is checked (dep root existence, not target binary existence), a `:::warning` is appropriate here.

- **`Python` mentioned in narrative prose without a link to python.org** — the closing sentence references "Python's [entry-points convention][python-entry-points]" which hyperlinks the specification. The word "Python" itself is not separately linked to python.org, but the linked text "entry-points convention" is the more useful destination. Per `docs-style.md`: "Every external tool mentioned must hyperlink." Since `python-entry-points` is the link and the word "Python" is embedded in the link phrase in the reference page (`reference/metadata.md:331`), this is marginal — the spec link covers it. However the authoring page's phrasing "Python's [entry-points convention]" puts the proper noun "Python" outside the link text. Flag as Warn; a tighter phrasing would be `[Python's entry-points convention][python-entry-points]`.

- **No `:::warning` or `:::tip` callout for the "must exist" constraint** — per `docs-style.md`, `:::warning` is for "important caveats, commonly misunderstood things." The exec-vs-install-time resolution distinction (launchers don't check binary existence at install time) is a genuine usability footgun. After the Inconsistent finding is fixed, the corrected statement belongs in a `:::warning` callout.

- **Narrative structure** — prose intro, two JSON samples with framing prose, closing sentence. Structure is clean per `docs-style.md`. No violation.

---

## website/src/docs/authoring/entry-points.md#when — When to Declare Entry Points

### Verified

- **`entrypoints` field name** — plural, lowercase, correct. Struct `Entrypoints` serialises transparently as JSON array under the key `"entrypoints"`. Source: `crates/ocx_lib/src/package/metadata/entrypoint.rs:122-126`, `crates/ocx_lib/src/package/metadata/bundle.rs`.

- **`ocx launcher exec` subcommand path** — exact. The hidden subcommand group is `launcher`, its sole sub-subcommand is `exec`. Wire form: `ocx launcher exec '<pkg-root>' -- <argv0> [args...]`. Source: `crates/ocx_cli/src/command/launcher/mod.rs:27-36`, `crates/ocx_cli/src/command/launcher/exec.rs:30-36`.

- **Clean-environment execution bullet** — accurate. `ocx launcher exec` forces `self_view=true` internally and builds the env from scratch from the package metadata before execing the binary. Ambient env is not inherited (only `OCX_*` config keys forwarded via `apply_ocx_config`). Source: `crates/ocx_cli/src/command/launcher/exec.rs:48-91`.

- **"Fifty binaries / three on PATH" bullet** — architecturally correct. Packages that declare entrypoints can omit `public` PATH entries for `bin/` and rely entirely on the synth-PATH mechanism. Source: `crates/ocx_lib/src/package_manager/composer.rs:558-568`, `website/src/docs/authoring/env-surface.md:101-103`.

- **"Demote `${installPath}/bin` from `public` to `private`"** — the tip at `env-surface.md:101-103` is real and covers this exact pattern. The cross-reference `[authoring-env-surface]` resolves to `./env-surface.md` (line 162), which exists. Verified: `website/src/docs/authoring/env-surface.md:101-103`.

- **`[authoring-env-surface]` link** — file exists at `website/src/docs/authoring/env-surface.md`. Link definition at entry-points.md:162 resolves correctly.

- **`[cmd-exec]` anchor `#exec`** — the anchor `{#exec}` exists at `website/src/docs/reference/command-line.md:313`. Link definition at entry-points.md:152 targets `../reference/command-line.md#exec`, which is correct. No `#launcher-exec` anchor exists in that file; the tip inside the `exec` section explains the difference. The link is appropriate — it points users to the `exec` command reference where the launcher-exec relationship is documented.

- **Skip paragraph (Go, Rust, no dynamic dep)** — matches architectural guidance. Source: `crates/ocx_lib/src/package_manager/launcher/body.rs:20-24`, `in-depth/entry-points.md#template:66`, mirrors/cmake README uses CMake as an entrypoints example, confirming CMake itself does benefit from entrypoints (runtime resolution caveat below does not affect the skip-for-static-binaries advice).

- **Bullets style with bolded leads** — consistent with `docs-style.md` conventions. No issues.

### Inconsistent / hallucinated [Block]

- **"The substitution happens once at install time and bakes into the launcher script"** (second bullet, deps runtime lookup): **false per implementation**. `crates/ocx_lib/src/package_manager/launcher/generate.rs:78-81` explicitly states: "Per ADR §6 the resolved value is NOT baked into the launcher — `ocx exec` re-resolves it from `metadata.json` at invocation time — so the result is discarded." The launcher body (both Unix `.sh` and Windows `.cmd`) contains only the baked package-root path; no resolved `target` or `${deps.*}` path appears in the script body. `crates/ocx_lib/src/package_manager/launcher/body.rs:20-24,62-66` confirms this explicitly. The `in-depth/entry-points.md:71` is also inconsistent with the code ("The resolved path is baked into the launcher body") — that page says the same false thing. The launcher resolves the binary at runtime via the composed PATH env (which includes deps' `bin/` directories via the env composition step), not by baking a resolved target path.

  **Impact**: The second bullet misrepresents how `${deps.NAME.installPath}` works. The template resolves at install time only as a *validation check* (fail-fast on malformed metadata). Nothing from the resolution is written into the launcher. The consumer-visible effect is still correct — the launcher does find the dependency's binary — but the mechanism is wrong (PATH env composition, not a baked resolved path).

### Missing nuance / drift [Warn]

- **Synth-PATH path: `<pkg-root>/entrypoints` vs `current/entrypoints`** — the doc says "The synthetic `PATH ⊳ <pkg-root>/entrypoints` entry that OCX adds at exec time". This is technically the composer's internal representation (absolute package-root path to `entrypoints/`). Users encounter it as `<symlink-root>/current/entrypoints` in their shell PATH (via `ocx shell profile load`). The doc's phrasing is not wrong but could confuse readers who see their PATH has `current/entrypoints`, not a raw content-addressed path. Source: `crates/ocx_lib/src/package_manager/composer.rs:558-568`, `website/src/docs/in-depth/entry-points.md:121-125`.

- **Self-view suppression of root's own entrypoints** — the clean-env bullet says the launcher "stripping ambient env so the package always runs with the variables it declared." That is accurate but misses a related nuance: under `self_view=true` (the forced mode inside `launcher exec`), the root's own `entrypoints/` synth-PATH is explicitly suppressed to prevent infinite recursion. This is an important execution safety property not mentioned anywhere in `#when`. Source: `crates/ocx_lib/src/package_manager/composer.rs:434-454`.

- **Windows `.cmd` launchers** — the section says "the launcher re-enters via `ocx launcher exec`" without mentioning that on Windows the launcher is a `.cmd` file using `IF DEFINED OCX_BINARY_PIN` branching rather than a shell variable expansion. This is a platform-specific detail but relevant for `#when`'s audience (publishers deciding whether to declare entrypoints). The in-depth page covers it (`in-depth/entry-points.md:83-93`), so the authoring guide's omission is borderline acceptable if the link to in-depth is clear.

### Broken refs [Block]

- (none) — both `[cmd-exec]` (`../reference/command-line.md#exec`) and `[authoring-env-surface]` (`./env-surface.md`) resolve to existing files and anchors.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none) — `#when` section has no code examples or JSON samples.

### Style / convention violations [Warn]

- **`Cmake` — miscapitalized** — line 37: "Cmake, ripgrep, mold". The correct proper noun is **CMake** (capital C and M). The OCX codebase consistently uses `CMake`: `mirrors/cmake/README.md:7,9`, `mirrors/cmake/mirror.yml:9` (`repo: CMake`), `website/src/docs/catalog/cmake.md:16,18`. The lowercased `c` is a typo.

- **External tools not hyperlinked** — per `docs-style.md` §"Real-World Examples", every external tool mentioned must hyperlink. The following tools in `#when` are mentioned without links:
  - Python (`website/src/docs/authoring/entry-points.md:32`) — no hyperlink to python.org or similar.
  - Node CLIs / Node — no hyperlink.
  - JVM tools — no hyperlink.
  - Ruby gems — no hyperlink.
  - Go binaries — no hyperlink.
  - Rust binaries — no hyperlink.
  - CMake — no hyperlink (cmake.org).
  - ripgrep — no hyperlink (github.com/BurntSushi/ripgrep).
  - mold — no hyperlink (github.com/rui314/mold).
  The `[python-entry-points]` link defined at entry-points.md:145 is used later in `#target` but not in `#when`.

- **`⊳` symbol** — the triangle/push symbol in "PATH `⊳` `<pkg-root>/entrypoints`" is non-standard jargon (borrowed from formal notation for PATH prepending). No tooltip or inline explanation is provided. The symbol is used consistently in the in-depth page and the reference but is never introduced or defined anywhere in the docs. This is a borderline `docs-style.md` violation (no tooltip per the Tooltip component guidance). Recommend wrapping in `<Tooltip term="PATH ⊳ dir">prepends `dir` to the front of PATH</Tooltip>` on first use per section.

---

## website/src/docs/authoring/entry-points.md#why — Why Encapsulate Through a Launcher

### Verified

- **`ocx launcher exec` exists** — `crates/ocx_cli/src/command/launcher/mod.rs` and `crates/ocx_cli/src/command/launcher/exec.rs` confirm a real hidden subcommand. Wire ABI is `ocx launcher exec '<pkg-root>' -- <argv0> [args...]` (launcher/mod.rs:31). Binary help output (`ocx launcher --help`) confirms: `exec  Execute an installed package entrypoint from a generated launcher`.

- **`content/` directory inside the package** — confirmed at `crates/ocx_lib/src/file_structure/package_store.rs:11` (`content/ -- the installed package files`) and `website/src/docs/in-depth/storage.md:54` (Tree node `<Node name="content/">`). Claim that the launcher carries "a baked path to the binary inside this package's `content/`" is accurate.

- **`[in-depth-environments]` link resolves** — link definition at entry-points.md:156 is `../in-depth/environments.md`; file exists at `website/src/docs/in-depth/environments.md` (18.5 KB).

- **`[cmd-exec]` link definition** — entry-points.md:152 maps `[cmd-exec]` to `../reference/command-line.md#exec`. Anchor `{#exec}` exists at command-line.md:313. No broken ref for the defined target itself.

- **`ocx launcher exec` noted at the `#exec` anchor** — command-line.md:321–322 includes a tip callout: "Generated launchers use `ocx launcher exec`, not `ocx exec`", with the correct wire ABI. The `#exec` section is therefore a reasonable (if imperfect) landing target for readers following `[cmd-exec]`.

- **`content/` is correct terminology** — package_store.rs consistently uses `content/` as the subdirectory name for package files; `metadata.json` and `manifest.json` live alongside it as siblings, not inside it. The doc phrasing "a baked path to the binary inside this package's `content/`" is accurate.

- **Narrative structure** — section opens with a vivid two-tool conflict scenario, then names the structural cause, then explains the solution. Matches docs-style.md §"Narrative Structure" (idea → problem → solution). Good.

- **`:::tip` callout** — valid VitePress callout type, bold label "Mental model" is fine per docs-style.md.

- **Italic emphasis** — `*its*`, `*their environment*`, `*executables*` used sparingly. OK.

### Inconsistent / hallucinated [Block]

- **"strips ambient env before exec" is false for the launcher path** — entry-points.md:20 says the launcher "strips ambient env before exec" via `ocx launcher exec`. The source code contradicts this: `launcher/exec.rs:81` calls `env::Env::new()`, which (per `env.rs:119–122`) initialises from `std::env::vars_os()` — i.e., it **inherits** the full ambient environment and then applies/overrides with package entries on top. Only `ocx exec --clean` uses `env::Env::clean()` (exec.rs:76), which starts from an empty map. The launcher never calls `Env::clean()`.

  Accurate description: "the launcher overlays the package's composed env on top of the inherited shell env — the package's own vars take precedence, but ambient vars that the package does not touch are still present." The phrase "strips ambient env" is a Block-tier factual error.

### Missing nuance / drift [Warn]

- **"Neither is reachable from the other's process tree"** — entry-points.md:22. The claim is imprecise. Because `launcher exec` inherits ambient env (see above), PATH and other ambient vars are present in both tool processes unless the package's own env entries explicitly shadow them. True isolation (no ambient env leakage) would require `Env::clean()`. What OCX actually guarantees is: each tool's declared dep is on its own PATH before the ambient PATH, and the dep is pinned by digest — so the *right* runtime wins the resolver race even if an ambient runtime is also present. The two process trees are not isolated from each other in the process-hierarchy sense either; they are independent exec chains with no parent-child relationship unless the user deliberately chains them. The more precise framing: "Neither tool's launcher exposes its pinned runtime as an ambient PATH entry that the consumer's shell or the other launcher can accidentally pick up." This is a Warn-tier precision issue but borders on Block given it directly contradicts the "strips ambient env" claim.

- **Composed environment description** — entry-points.md:19 says the launcher carries "its own env entries plus the env contributed by its declared dependencies." This is accurate but incomplete: it is specifically the *private surface* env (self-view, `has_private()` = true), not the interface surface. The sentence could mislead a publisher who thinks the consumer-visible interface entries are what's composed in the launcher; the launcher uses `resolve_env(..., true)` (launcher/exec.rs:51) which forces self-view. Warn-tier imprecision.

### Broken refs [Block]

- **`[cmd-exec]` resolves to `command-line.md#exec`, not `command-line.md#launcher-exec`** — There is no `{#launcher-exec}` anchor in the reference docs. The `#exec` section documents `ocx exec`, not `ocx launcher exec`. A reader following the link to understand the "re-entry through `ocx launcher exec`" mechanism lands on the public `exec` subcommand page, which says different things (auto-install, `--clean` flag, etc.). The tip callout at command-line.md:321–322 partially covers this, but there is no dedicated anchor for `launcher exec`. This is a Warn-tier ref mismatch rather than a dead link, but it is misleading enough to note: the linked anchor (`#exec`) does not document the command cited in the body text (`ocx launcher exec`).

### Example/cast/JSON sample issues

- (none)

### Style / convention violations [Warn]

- **External tools not hyperlinked** — docs-style.md §"Real-World Examples and External Links" requires "every external tool mentioned must hyperlink — every occurrence, not just first." The section mentions: **Node** (no link), **Java** (no link), **JDK** (no link). None are hyperlinked in the `#why` section or its bullet list. Canonical links would be: [Node.js](https://nodejs.org/), [Java](https://www.java.com/), [JDK](https://openjdk.org/) (or a specific distribution). This is a Warn-tier style violation per docs-style.md.

- **`bin/` directories** (entry-points.md:12) also mentioned without link; however `bin/` is a filesystem convention, not an external tool, so no link required. Not a violation.

---

## website/src/docs/authoring/index.md#decisions — Decision Flowchart

Fact-checked 2026-05-07 against sources listed below.

### Verified

- **Prose lead-in exists** (`index.md:79`): "Common questions you will hit while authoring, and the page that answers each:" — one sentence, present.

- **Table column headers**: `| Question | Page |` — consistent with docs-style.md Q→Page convention.

- **`authoring-bundle-anatomy-strip` → `./bundle-anatomy.md#strip-components`** (`index.md:124`): anchor `{#strip-components}` exists at `bundle-anatomy.md:80`. Heading: "Stripping Upstream Wrappers". Row question ("Should I repack the upstream archive or use `strip_components`?") maps correctly to that section's content.

- **`strip_components` field name is correct**: confirmed in Rust source at `crates/ocx_lib/src/package/metadata/bundle.rs:51` (`strip_components: Option<u8>`) and in `reference/metadata.md:35` (top-level table) and `metadata.md:367` (`{#extraction-strip-components}`).

- **`authoring-env-visibility` → `./env-surface.md#visibility`** (`index.md:128`): anchor `{#visibility}` exists at `env-surface.md:25`. Heading: "Choosing Visibility". Row question ("Which env vars should consumers see vs. only my launchers?") matches content accurately.

- **`authoring-entry-points-when` → `./entry-points.md#when`** (`index.md:131`): anchor `{#when}` exists at `entry-points.md:28`. Heading: "When to Declare Entry Points". Row question ("When do I need named entry points instead of `PATH += bin/`?") matches content accurately.

- **`authoring-deps-name` → `./dependencies.md#name-field`** (`index.md:126`): anchor `{#name-field}` exists at `dependencies.md:37`. Heading: "When You Need a `name` Override". Row question ("When do I need a `name` override on a dependency?") matches content accurately.

- **`name` field on dependencies is correct**: confirmed in `crates/ocx_lib/src/package/metadata/dependency.rs:106` (`pub name: Option<DependencyName>`) and in `reference/metadata.md:204` (dependencies table row for `name`).

- **`authoring-cascade` → `./building-pushing.md#cascade`** (`index.md:133`): anchor `{#cascade}` exists at `building-pushing.md:26`. Heading: "Cascading Rolling Tags". Row question ("How do I make rolling tags (`1.0`, `1`, `latest`) follow new releases?") matches content accurately.

- **`--cascade` is the actual flag**: confirmed in `crates/ocx_cli/src/command/package_push.rs:18` (`#[clap(long = "cascade", short = 'c')]`) and in `reference/command-line.md:853` (`-c`, `--cascade`).

- **`authoring-multi-platform` → `./multi-platform.md`** (`index.md:135`): page exists. Row question ("How do I publish for amd64 + arm64 + darwin under one tag?") matches the page's content (per-platform push pattern assembling OCI Image Index).

- **`authoring-layer-reuse` → `./building-pushing.md#layer-reuse`** (`index.md:134`): anchor `{#layer-reuse}` exists at `building-pushing.md:35`. Heading: "Reusing Layers Across Packages". Row question matches content accurately.

- **`authoring-describe` → `./migration.md#describe`** (`index.md:137`): anchor `{#describe}` exists at `migration.md:66`. Heading: "Attaching Description Metadata". Row question ("How do I add a README or logo to a published package?") matches content accurately — section describes `ocx package describe --readme` and `--logo`.

- **`package describe` is the correct command**: confirmed in `reference/command-line.md:876` (`{#package-describe}`), and anchor referenced in the table at `index.md:96` as `[reference-cli-package]: ../reference/command-line.md#package`.

- **`authoring-env-migrating` → `./env-surface.md#migrating`** (`index.md:129`): anchor `{#migrating}` exists at `env-surface.md:52`. Heading: "Migrating from Implicitly Public". Row question ("How do I retrofit an older `metadata.json` for entry visibility?") matches content accurately.

- **Style compliance**: table uses reference-style links collected at bottom of file (`index.md:122–138`), no inline `[text](url)` in body. Conforms to `docs-style.md` link-syntax rule.

- **Row descriptions match destination section content**: all nine rows verified — destination heading and prose correspond to the question asked.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **Row for `authoring-describe` says "Migration patterns → describe"** (`index.md:90`), but the section it links to (`migration.md#describe`) is titled "Attaching Description Metadata" — not a migration pattern in the narrow sense. The linked page is `migration.md`, so the "Migration patterns" framing is technically correct as a subsection name match, but the description work is not a migration concern for existing packages. The `#describe` anchor sits inside `migration.md` because that is where third-party wrapping patterns live; the row question phrasing ("How do I add a README or logo") accurately describes what the section does. Drift is low but the placement in "Migration patterns" may mislead first-time readers into thinking this is only for migrated packages — a note in the linked section or a `:::tip` could clarify. No link is broken; severity: Warn (framing drift, not incorrect content).

- **Row label for cascade says "Building & pushing → cascade"** but `multi-platform.md:48` also references `--cascade` in context of multi-platform releases. The single cascade row in the flowchart correctly points at `building-pushing.md#cascade`, but a reader seeking "how does cascade work with multi-platform?" would land in the right place only after reading cross-references. This is acceptable given the table's purpose (one-question → one-page routing), but the multi-platform interaction is worth a cross-reference in `building-pushing.md#cascade`. No broken link; severity: Warn (missing cross-reference, not a broken ref).

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none) — no inline examples in the Decision Flowchart section itself.

### Style / convention violations [Warn]

- (none) — the section opens with a single-sentence prose lead-in, uses a two-column `| Question | Page |` table with reference-style links, and follows `docs-style.md` conventions throughout.

---

**Sources verified against:**
- `website/src/docs/authoring/index.md` (link definitions: lines 122–138)
- `website/src/docs/authoring/bundle-anatomy.md` (anchor `#strip-components`)
- `website/src/docs/authoring/env-surface.md` (anchors `#visibility`, `#migrating`)
- `website/src/docs/authoring/entry-points.md` (anchor `#when`)
- `website/src/docs/authoring/dependencies.md` (anchor `#name-field`)
- `website/src/docs/authoring/building-pushing.md` (anchors `#cascade`, `#layer-reuse`)
- `website/src/docs/authoring/multi-platform.md` (page-level)
- `website/src/docs/authoring/migration.md` (anchor `#describe`)
- `website/src/docs/reference/metadata.md` (anchors `#extraction-strip-components`, `#dependencies-entry`)
- `website/src/docs/reference/command-line.md` (anchor `#package-describe`)
- `crates/ocx_lib/src/package/metadata/bundle.rs` (field `strip_components`)
- `crates/ocx_lib/src/package/metadata/dependency.rs` (field `name`)
- `crates/ocx_cli/src/command/package_push.rs` (flag `--cascade`)
- `.claude/rules/docs-style.md` (style conventions)

---

## website/src/docs/authoring/index.md#journey — The Publisher Journey

### Verified

- **All seven sibling files exist.**
  - `website/src/docs/authoring/bundle-anatomy.md` — exists
  - `website/src/docs/authoring/dependencies.md` — exists
  - `website/src/docs/authoring/env-surface.md` — exists
  - `website/src/docs/authoring/entry-points.md` — exists
  - `website/src/docs/authoring/building-pushing.md` — exists
  - `website/src/docs/authoring/multi-platform.md` — exists
  - `website/src/docs/authoring/migration.md` — exists

- **`--cascade` flag name is correct.** Source: `crates/ocx_cli/src/command/package_push.rs:18` — `#[clap(long = "cascade", short = 'c')]`. The CLI reference at `website/src/docs/reference/command-line.md:853` also confirms `-c`, `--cascade`.

- **Dependency edge visibility uses a four-value enum (`sealed`, `private`, `public`, `interface`).** Confirmed at `website/src/docs/authoring/dependencies.md:56–65` (`#edge-visibility` section, table with all four values and their semantics). This matches the claim in the journey bullet. The claim that the page covers "what visibility to choose for the dependency edge" is accurate.

- **OCI Image Index link target is canonical.** The link definition at `website/src/docs/authoring/index.md:103` resolves to `https://github.com/opencontainers/image-spec/blob/main/image-index.md`, which is the canonical OCI image-spec GitHub repository. This matches the recommendation in `docs-style.md:51`.

- **Bundle anatomy bullet summary is accurate.** The page (`bundle-anatomy.md`) covers what goes in the archive (`#what-goes-in`), whether to repack the upstream layout (with `strip_components`), and compression choices (`#compression`). The "Reach for this" trigger matches the page's actual content.

- **Dependencies bullet summary is accurate.** The page (`dependencies.md`) covers when to declare (`#when`), pinning by digest (`#pinning`), and edge visibility (`#edge-visibility`). The "Reach for this when your tool needs to find another tool on disk at runtime" trigger matches the page's `#when` section.

- **Entry points bullet summary is accurate.** The page (`entry-points.md`) covers when to declare (`#when`), naming and collisions (`#naming`), and `target` templates threading dependency paths (`#target`). The Python/Node/JVM "Reach for this" trigger matches the page's worked example (`#python-example`) and `#when` section.

- **Building and pushing bullet summary is accurate.** The page (`building-pushing.md`) has sections: first push (`#first-push`), BYO archives (`#byo-archives`), cascade (`#cascade`), and layer reuse (`#layer-reuse`). All four elements in the bullet are confirmed.

- **Multi-platform bullet summary is accurate.** The page (`multi-platform.md`) covers pushing per platform under one tag and OCI Image Index assembly (`#concept`, `#pattern`). The "Reach for this when supporting more than one OS/arch" trigger matches the page intro.

- **Migration patterns bullet summary is accurate.** The page (`migration.md`) covers `ocx_mirror` specs (`#mirror`), repackaging GitHub Releases (`#github-releases`), repackaging Homebrew (`#homebrew`), and attaching description metadata (`#describe`). All four elements confirmed. `ocx_mirror` spelling is consistent.

- **"Seven decisions" count is accurate.** Lines 69–75 of `index.md` list exactly seven bullets.

- **Style — "Reach for this when …" descriptors rescue the list from dump-list classification.** Each bullet carries a "Reach for this when …" clause that contextualizes the link in terms of a real publisher scenario. `docs-style.md:31` prohibits "dump lists of commands without explaining what they represent." These are not bare links or bare commands — they include two-part descriptions (summary + trigger). The list does not violate the dump-list rule.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **Env surface bullet says "which to mark `public` versus `private`" — omits the third value `interface`.** The actual `env-surface.md` page (`env-surface.md:32`) says "Three values map onto the two surfaces" and documents `private`, `public`, and `interface`. The journey bullet reduces this to a binary (`public` versus `private`), which is an understatement that may cause publishers to overlook `interface` when it is the right choice (e.g., for `PKG_CONFIG_PATH` or `MANPATH`). The tip at `env-surface.md:43` reinforces that most typical vars are public, so the simplification is defensible for a teaser — but `interface` is a named concept the page teaches, and its omission drifts from the page's actual surface. **Severity: Warn** — the bullet does not say "only two values"; it simply names the two most common ones. Readers will discover `interface` on the linked page, so this is a narrowing, not a falsehood.

- **Env surface bullet says "from before entry visibility existed" — the page (`env-surface.md:54`) says the feature arrived with the "entry-points feature", not a standalone "entry visibility" feature.** The bullet's phrasing implies a separate historical milestone. The page ties migration squarely to the entry-points shipping event. Minor drift in framing; no information is lost, but the terminology is slightly inconsistent with the page.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none — the `#journey` section contains no code blocks or JSON samples; it is prose + bullet list only)

### Style / convention violations [Warn]

- **"Seven decisions" is a mild count-marketing open.** `docs-style.md:22` states: "No sales pitch or marketing open." The phrase "splits into seven decisions" is borderline: it is a factually accurate structural statement (there are seven bullets), not a superlative or slogan. It does not match the prohibited example "One identifier. All platforms. No hash list." The rule targets puffery, not structural framing. **Verdict: borderline but defensible** — noting as Warn because the number framing ("seven") is faint count-advertising, and the section could open with "The publisher decisions split across seven pages:" without the slight flourish.

- **Section opens with a single framing sentence ("Most packages start as …") then immediately drops into a list, with no "The problem" or "The solution" paragraph structure.** `docs-style.md:14–18` (Narrative Structure) calls for two–three short paragraphs: idea → problem → solution. The `#journey` section has one framing sentence and then the bullet list. However, the rule is written for `##` sections as a general guide; the `#journey` section is explicitly an overview/navigation table, and a paragraph narrative before each link would bloat an index section. The "Reach for this when …" clauses substitute for the "why" paragraphs inline. **Verdict: minor violation** — the section is a navigational overview, and the "Reach for this" clauses partially satisfy narrative intent, but the literal Narrative Structure rule is not met.

---

## website/src/docs/authoring/index.md#tldr — TL;DR — Publish a Binary

### Verified
- `ocx install` accepts a fully-qualified `registry/repo:tag` argument — evidence: `crates/ocx_lib/src/oci/identifier.rs:449-466` (test `parse_with_registry` parses `test.com/repo:tag`; `parse_internal` handles domain-prefixed names); `ocx install --help` confirms `<PACKAGES>...` positional.
- `ocx package create` flag `-m`/`--metadata` exists — evidence: `./target/release/ocx package create --help`.
- `ocx package create` flag `-o`/`--output` exists — evidence: `./target/release/ocx package create --help`.
- `ocx package create` accepts positional `<PATH>` (directory path) — evidence: `./target/release/ocx package create --help`.
- `ocx package push` flag `-n`/`--new` exists — evidence: `./target/release/ocx package push --help` ("Indicates that this is a new package that doesn't exist in the registry yet").
- `ocx package push` flag `-p`/`--platform` exists — evidence: `./target/release/ocx package push --help`.
- `ocx package push` flag `-m`/`--metadata` exists — evidence: `./target/release/ocx package push --help`.
- `ocx package push` positional argument order `<IDENTIFIER> [LAYERS]...` is correct — evidence: `./target/release/ocx package push --help` ("Usage: ocx package push [OPTIONS] --platform <PLATFORM> <IDENTIFIER> [LAYERS]...").
- `$schema` URL `https://ocx.sh/schemas/metadata/v1.json` matches schema `$id` — evidence: `website/src/public/schemas/metadata/v1.json:228` (`"$id": "https://ocx.sh/schemas/metadata/v1.json"`).
- `type: "bundle"` is a valid schema discriminant — evidence: `website/src/public/schemas/metadata/v1.json:235-244` (`"const": "bundle"`).
- `version: 1` is valid — evidence: `website/src/public/schemas/metadata/v1.json:212-217` (`Version` enum with `[1]`).
- `env[].key` is a valid field — evidence: `website/src/public/schemas/metadata/v1.json:196-199` (`"key"` property on `Var`).
- `env[].type: "path"` is valid — evidence: `website/src/public/schemas/metadata/v1.json:166-179` (`"const": "path"`).
- `env[].value` is valid for path type — evidence: `website/src/public/schemas/metadata/v1.json:145-148`.
- `env[].visibility: "public"` is valid for env entries (`EntryVisibility`) — evidence: `website/src/public/schemas/metadata/v1.json:92-98` (`EntryVisibility` enum contains `"public"`).
- `${installPath}` placeholder is the correct template token — evidence: `crates/ocx_lib/src/package/metadata/env.rs:155`, `crates/ocx_lib/src/package/metadata/entrypoint.rs:108-109`.
- Metadata is uploaded as the OCI manifest config blob — evidence: `crates/ocx_lib/src/oci/client.rs:301,318,503` (config blob fetch/push path).
- `metadata.json` is written as a sibling of `content/` inside the package directory, not literally "next to the extracted layer" — evidence: `crates/ocx_lib/src/file_structure/package_store.rs:38-44` (`content()` → `dir/content`, `metadata()` → `dir/metadata.json`); both are children of the package root dir.
- Cast file `package-push.cast` exists — evidence: `website/src/public/casts/package-push.cast` (2.3K).
- `[cmd-install]` resolves to `../reference/command-line.md#install` — anchor `{#install}` exists at line 453.
- `[cmd-package-push]` resolves to `../reference/command-line.md#package-push` — anchor `{#package-push}` exists at line 830.
- `[authoring-env-visibility]` resolves to `./env-surface.md#visibility` — anchor `{#visibility}` exists at `website/src/docs/authoring/env-surface.md:25`.
- `[oci-manifest-config]` link target `https://github.com/opencontainers/image-spec/blob/main/manifest.md#image-manifest` is declared in file at line 104.
- `[authoring-dependencies]` resolves to `./dependencies.md` — file exists.
- `[authoring-entry-points]` resolves to `./entry-points.md` — file exists.
- `<Tree :collapsible="false">` prop is valid — evidence: `subsystem-website.md` documents `<Tree>` with `collapsible?: boolean` prop.
- `:::tip` callout type is correct per docs-style.md for actionable advice about recommended patterns — evidence: `docs-style.md:91-99`.

### Inconsistent / hallucinated [Block]
- Claim: "OCX restores it next to the extracted layer at install time" — actual: metadata.json is restored as a sibling of `content/` in the package directory (`packages/{registry}/{digest}/metadata.json`), not next to the extracted layer in `layers/`. The extracted layer lives separately in `layers/{registry}/{digest}/content/`. The phrasing "next to the extracted layer" is architecturally misleading — the layer store and package store are distinct tiers. (evidence: `crates/ocx_lib/src/file_structure/package_store.rs:38-44`, `crates/ocx_lib/src/file_structure/subsystem-file-structure.md` three-tier CAS description) → fix: replace "next to the extracted layer" with "as a sibling of `content/` in the assembled package directory".

### Missing nuance / drift [Warn]
- The cast (`package-push.cast`) shows the push using `mytool:1.0.0` (no registry prefix) against what appears to be a local/default registry, but the prose example uses `ghcr.io/me/mytool:1.0.0` — minor inconsistency between the recorded terminal and the prose. Not a bug but consumers may be confused seeing a different identifier in the cast vs. the code snippet. → fix: either note the cast uses the default registry for brevity, or regenerate the cast to match the full ghcr.io reference.
- The metadata JSON example omits the `required` field on the `Path` type (which defaults to `false`). This is fine since the field has a default, but a reader of the schema may wonder if it is needed. → fix: (none required, but could add a comment or tooltip noting `required` defaults to `false`).
- The `ocx install` reference (command-line.md:453-477) documents the `-s`/`--select` flag but the TL;DR prose says "the binary lands on their PATH" — this is only true if consumers also set PATH entries from the candidate symlink or use `ocx shell env`. The `ocx install` step alone only creates a candidate symlink; the PATH exposure depends on how the consumer activates the environment. → fix: clarify that `ocx install` installs and creates the candidate symlink, and consumers use `ocx shell env` or `ocx exec` to resolve PATH, or note that the env entry lands on PATH when the package environment is activated.

### Broken refs [Block]
- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]
- [Warn] The cast title in the frontmatter is `"Publishing a package"` but the `<Terminal>` prop sets `title="Bundle and push mytool:1.0.0"`. The terminal component title overrides the cast header, so this is harmless but inconsistent metadata. (evidence: `website/src/public/casts/package-push.cast:1`)
- [Warn] The cast shows the command sequence `ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz` followed by `ocx package push -n -p linux/amd64 -m metadata.json mytool:1.0.0 mytool-1.0.0.tar.xz` (no ghcr.io registry prefix) and then `ocx index update mytool` + `ocx index list mytool` — the extra index commands do not appear in the prose snippet above the cast, so the terminal shows more steps than the prose describes. Minor drift, not a correctness error.

### Style / convention violations [Warn]
- The `:::tip` callout body starts "If your 'binary' is a Python script..." — the opening sentence summarizes what the rest of the tip expands on, which is valid. However, `docs-style.md` says `:::info` is for analogies/background context and `:::tip` for actionable advice. The scripted runtimes tip is a mix of background context (Python, JAR, interpreter) and actionable advice (declare dep, ship entry point). The actionable part is the closing sentences; the opening is background context. Acceptable use of `:::tip` but borderline — the analog to another ecosystem (shebang/JAR) could fit an `:::info` pattern per `docs-style.md:63-64`. → fix: consider splitting or leading with the actionable recommendation before the explanatory context; or leave as-is (borderline, not a hard violation).

---

## website/src/docs/authoring/migration.md#describe — Attaching Description Metadata

### Verified

- **`--readme`, `--title`, `--description` flags exist** — confirmed in `./target/release/ocx package describe --help` and source at `crates/ocx_cli/src/command/package_describe.rs:21-34`.
- **`--logo` flag exists** — `--logo <LOGO>` accepted (PNG or SVG); source line 25. The prose says "publishers attach a README, logo, and search keywords" which is accurate as capabilities.
- **`--keywords` flag exists** — `--keywords <KEYWORDS>` (comma-separated, sets `sh.ocx.keywords`); source line 37. The prose lists "search keywords" as a named capability, which is correct, but the code example omits `--keywords` entirely. No inaccuracy in what is said, but the example is incomplete relative to the narrative setup.
- **`ocx package info --save-readme` flag exists** — confirmed via `./target/release/ocx package info --help`. Also `--save-logo` exists (not mentioned in prose — completeness gap only, not an error).
- **Cast file exists** — `/website/src/public/casts/package-describe.cast` (3.1K). Content includes `ocx package describe --readme README.md --title "mytool" --description "A small example tool" mytool` and `ocx package info mytool` — matches the code fence exactly.
- **Cast title** — cast header contains `"title": "Attaching package descriptions"` (not "Attaching package descriptions and reading them back" as in `<Terminal>` prop); the `title` prop on `<Terminal>` is a UI label, not the cast header, so this is fine — no inaccuracy.
- **`[cmd-package-describe]` link** resolves to `../reference/command-line.md#package-describe` — anchor `#package-describe` confirmed at `website/src/docs/reference/command-line.md:876`.
- **`[cmd-package-info]` link** resolves to `../reference/command-line.md#package-info` — anchor `#package-info` confirmed at line 901.
- **`[catalog]` link** resolves to `../catalog.md` — file exists at `website/src/docs/catalog.md`.
- **Code example commands and flags** are correct: `--readme README.md --title "mytool" --description "A small example tool" mytool` matches the CLI exactly. `ocx package info mytool` is also valid.
- **Reference-style links** used throughout — matches docs-style.md convention.

### Inconsistent / hallucinated [Block]

- **"referrer manifest pointing at the package"** — HALLUCINATED. The description artifact is NOT pushed as an OCI referrer via the Referrers API. It is pushed as an OCI image manifest to the `__ocx.desc` tag (`InternalTag::DESCRIPTION_TAG = "__ocx.desc"`), a tag on the same repository. This is a tag-based mechanism, not a referrer attachment. Evidence:
  - `crates/ocx_lib/src/oci/client.rs:668`: `/// Pushes a description artifact to the __ocx.desc tag.`
  - `crates/ocx_lib/src/package/tag.rs:26`: `pub const DESCRIPTION_TAG: &str = "__ocx.desc";`
  - The phrase "it travels with the registry export" is unverifiable — no export/mirror path for description blobs was found in the pipeline.
  - The word "referrer" does not appear anywhere in `crates/ocx_lib/src/oci/client.rs` or any other source file. The term "referrer manifest" is technically incorrect for this implementation.

- **"`ocx_mirror`'s pipeline runs `ocx package describe` automatically when the spec carries `description`, `readme`, or `logo` fields"** — HALLUCINATED. The `ocx_mirror` sync pipeline (`crates/ocx_mirror/src/command/sync.rs`, `crates/ocx_mirror/src/pipeline/orchestrator.rs`, `crates/ocx_mirror/src/pipeline/push.rs`) does NOT invoke `ocx package describe`, `Publisher::push_description`, or any description-pushing logic. A full search of all `.rs` files in `crates/ocx_mirror/src/` found zero references to `push_description`, `describe`, `readme`, `logo` (in any code-relevant sense), or `Publisher::push_description`. The mirror spec YAML (`MirrorSpec`) has no `description`, `readme`, or `logo` fields. The mirrors in `mirrors/*/README.md` have frontmatter `description:` and `keywords:` fields but these are README frontmatter parsed by `ocx package describe` when invoked manually — they are NOT read by `ocx_mirror` at all.

### Missing nuance / drift [Warn]

- **`--keywords` flag omitted from code example** — The prose says "publishers attach a README, logo, and **search keywords**" but the code fence and the cast only use `--readme`, `--title`, `--description`. The `--keywords` flag (present in source) is never demonstrated. A reader following the example will not know how to set keywords. The example should include `--keywords` or the narrative should acknowledge it separately.

- **`--logo` flag omitted from code example** — Prose says "README, logo, and search keywords" but the example only shows `--readme`. Neither `--logo` nor `--keywords` appear in the example or the cast script (`test/recordings/scripts/package-describe.sh` line 5). The prose setup promises three capabilities but the example only shows one.

- **`--save-logo` on `ocx package info` unmentioned** — minor completeness gap only; prose focuses on `--save-readme` which works correctly. Not a factual error.

- **Mirror auto-describe claim contradicted by code** — marked [Block] above due to factual incorrectness, but also worth noting as [Warn]: the actual pattern used in `mirrors/*/README.md` (frontmatter with `description:` and `keywords:`) is parsed by `package::description::parse_readme` in `package_describe.rs:74` when `--readme README.md` is passed. So the README frontmatter → metadata path exists, but it requires a manual `ocx package describe --readme README.md mytool` invocation; it is NOT automatic in the mirror pipeline.

### Broken refs [Block]

- (none) — all three link targets verified to exist (`#package-describe`, `#package-info`, `catalog.md`).

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **Cast content matches code fence** — commands in the cast match the example exactly. No mismatch.
- **[Warn] Code example omits `--keywords` and `--logo`** — the prose promises three capabilities (README, logo, keywords) but the example demonstrates only one (README). Weakens the instructional value.

### Style / convention violations [Warn]

- **[Warn] The `ocx_mirror` integration paragraph warrants a `:::tip` callout** — per `docs-style.md`, actionable advice and recommended patterns belong in `:::tip` boxes. The cross-cutting claim about mirror auto-running `describe` is actionable (workflow advice). Regardless of the factual fix needed, the corrected content should be a `:::tip` ("When using `ocx_mirror`, call `ocx package describe` once after initial publish; subsequent mirror runs do not overwrite it") rather than inline prose.

- **[Warn] The narrative promises three features (README, logo, keywords) in the opening sentence but demonstrates only one in the example.** Per `docs-style.md` "concrete command sequences" principle, the example should cover what the prose claims, or the prose should be scoped to what the example covers.

---

## website/src/docs/authoring/migration.md#github-releases — Repackaging GitHub Releases

### Verified

- **`github_release` source type exists** — `crates/ocx_mirror/src/spec/source.rs:14` declares `GithubRelease` variant on the `Source` enum with `#[serde(tag = "type", rename_all = "snake_case")]`, which maps to the YAML key `github_release`. Confirmed.

- **`verify.github_asset_digest: true` field path** — `crates/ocx_mirror/src/spec/verify_config.rs:9` has `pub github_asset_digest: bool` with `#[serde(default = "default_true")]`. The parent `MirrorSpec` in `spec.rs:76` holds `pub verify: Option<VerifyConfig>`, serialized as the top-level `verify:` key. YAML path `verify.github_asset_digest` is correct. `mirrors/cmake/mirror.yml:51` uses exactly this path. Confirmed.

- **Multiple regex per platform** — `crates/ocx_mirror/src/spec/assets.rs:15–17` defines `AssetPatterns { patterns: HashMap<Platform, Vec<String>> }`. Each platform maps to a `Vec<String>` of patterns. Confirmed.

- **cmake mirror.yml uses exactly the example regexes** — `mirrors/cmake/mirror.yml:13–15` is word-for-word identical to the code fence in the doc. Confirmed.

- **`[gh-releases]` link** — `website/src/docs/authoring/migration.md:92` resolves to `https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases`. Valid GitHub Docs URL. Confirmed.

- **`[mirror-cmake]` link** — `website/src/docs/authoring/migration.md:98` resolves to `https://github.com/ocx-sh/ocx/blob/main/mirrors/cmake/mirror.yml`. Confirmed present on `main` branch (file exists at `mirrors/cmake/mirror.yml`).

- **Deterministic ordering across runs** — `crates/ocx_mirror/src/pipeline/orchestrator.rs:79–88` sorts versions oldest-first using `Version::parse` before building the push task list. Push phase is sequential in that order (`orchestrator.rs:173–239`). The per-run sort is deterministic by semver. Confirmed.

- **"Two CMake-style worked examples"** — the code fence shows one platform (`linux/amd64`) with two regexes. The heading says "Two CMake-style worked examples" — this means two regex patterns as examples, not two platform entries. In context that reading works, though it borders on ambiguous phrasing.

- **Style: prose intro + advantage + code fence + pointer** — structure matches docs-style.md conventions. OK.

- **Link syntax** — reference-style links with definitions at file bottom, grouped with comments. Compliant with `docs-style.md:113–140`.

### Inconsistent / hallucinated [Block]

- **"feeds them straight into `ocx package create` and `ocx package push`"** — this is incorrect. `ocx_mirror` does NOT shell out to `ocx package create` / `ocx package push`. The pipeline calls OCX lib internals directly: `publisher.push_cascade()` / `publisher.push()` (`pipeline/push.rs:38,68`) via the `Publisher` struct imported from `ocx_lib::publisher`. There is no subprocess invocation of `ocx package` commands anywhere in `crates/ocx_mirror/src/**`. The claim implies a shell-script-like composition that does not exist. **Block — hallucinated subprocess interface.**

- **"picks whichever the upstream actually published in that release"** — the actual behavior is different and more nuanced. `resolver::resolve_assets()` (`resolver.rs:20–60`) applies ALL patterns against ALL asset names and collects a `HashSet` of matches. If exactly 1 distinct asset name matches across all patterns, it is resolved. If 2+ distinct assets match, the platform is flagged `Ambiguous` (an error condition). The doc implies the pipeline selects one regex winner per release. In reality, for old CMake releases only the `Linux` (capital) pattern will match (the `linux` pattern yields 0 matches); for new releases only the `linux` pattern matches — resulting in exactly 1 total match either way. The mechanism is not "pick whichever" but "union of all matches must be exactly one". The described behavior is true for the CMake case but the underlying mechanism is misrepresented. **Block — mechanism is wrong (union-dedup, not priority pick).**

### Missing nuance / drift [Warn]

- **`github_asset_digest` conditional on digest availability** — `pipeline/verify.rs:89–93` shows `config.github_asset_digest` is only acted upon when `asset_digests.get(asset_name)` returns `Some`. The `asset_digests` map is passed as `&HashMap::new()` from `orchestrator.rs:301–302` — an empty map. This means `github_asset_digest: true` currently has no practical effect (no digests are populated from the GitHub API response into `asset_digests`). The doc implies `verify.github_asset_digest: true` actively verifies digests, which is aspirational/partially implemented. **Warn — misleading about current effectiveness of the flag.**

- **CMake naming case change at 3.20.0** — the mirror spec (`mirrors/cmake/mirror.yml:14`) itself annotates `# >= 3.20.0` for lowercase and `# < 3.20.0` for uppercase `Linux`. This is the authoritative in-tree record. No web search was performed to confirm the upstream CMake release history. The boundary version claim (3.20.0) is sourced from the in-tree spec; if the spec is correct, so is the doc. But no independent verification is possible within read-only scope. **Warn — boundary version not independently verified; doc cites spec, spec may be authoritative.**

- **"deterministic ordering it guarantees across runs"** — the ordering of platforms within a version group is not guaranteed; only version ordering is deterministic (oldest-first). Within a version, platforms come from a `HashMap` which has non-deterministic iteration order. For the cascade claim this is irrelevant (platforms are pushed in whatever order, cascade only cares about version order), but the blanket "deterministic ordering" claim oversimplifies. **Warn — ordering is deterministic for versions, not for platforms within a version.**

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **Code fence YAML is structurally valid** — the two-entry list under `linux/amd64:` matches the actual `AssetPatterns` deserialization shape (`HashMap<Platform, Vec<String>>`). Confirmed against `assets.rs:21`. OK.

- **"Two CMake-style worked examples"** — the fence shows only one platform (`linux/amd64`). The cmake spec has six platforms (`mirrors/cmake/mirror.yml:13–29`). The label "two … examples" refers to the two patterns under one platform, which is technically fine but could confuse readers who expect two separate platform examples. **Warn — "two … examples" label is ambiguous; could read as two platform entries.**

### Style / convention violations [Warn]

- **"Two CMake-style"** — should be "Two CMake-style" (already correct casing; no issue). The body text at line 50 reads "Two CMake-style worked examples" — "CMake" is correct casing. No typo.

- **`curl` not linked** — `docs-style.md:47` requires every external tool mentioned to hyperlink. `curl` appears in the body ("a few `curl` calls and a script") without a hyperlink. **Warn — external tool `curl` missing hyperlink.**

---

## website/src/docs/authoring/migration.md#homebrew — Repackaging Homebrew Formulae

### Verified

- **"Formulae are Ruby DSL programs, not declarative manifests"** — accurate. Homebrew docs (https://docs.brew.sh/Formula-Cookbook) explicitly state a formula is "a package definition written in Ruby" that leverages the `Formula` class API as a Ruby DSL. Every formula has its own `install` method and custom build logic, confirming the "not declarative manifests" characterisation.

- **`[homebrew]` link resolves to `https://brew.sh/`** — confirmed at `migration.md:93`. The URL is correct and the link uses reference-style syntax as required by `docs-style.md`.

- **`strip_components: 1` is a real, documented metadata field** — confirmed at `crates/ocx_lib/src/package/metadata/bundle.rs:51` (`pub strip_components: Option<u8>`). The Rust doc comment reads "Number of leading path components to strip when extracting the bundle." Behaviour confirmed by the extraction implementation at `crates/ocx_lib/src/archive/tar.rs` and documented in `website/src/docs/reference/metadata.md:367-377`.

- **"Source-built Homebrew formulae do not migrate cleanly without rebuilding, since OCX is a binary package manager"** — accurate. OCX is explicitly a binary-only package manager (product-context.md, competitive table). Source-built formulae produce compiled artefacts that embed install-time paths specific to Homebrew's Cellar layout; those paths cannot be trivially reused in OCX's content-addressed store without recompilation.

- **"The migration pattern is to read the formula's `url` and `sha256`"** — accurate for source-formula fetching. `url` and `sha256` are confirmed core formula fields (Formula Cookbook). The text intentionally simplifies (see item 7 below).

- **"`github_release`-shaped pipeline" is a real pipeline type** — confirmed in `crates/ocx_mirror/src/spec.rs` (numerous occurrences) and documented in `.claude/rules/subsystem-mirror.md:68`. The text's use of "shaped" (i.e., structurally similar, not necessarily using the `github_release` source type directly) is accurate — a Homebrew formula pointing to a GitHub-hosted archive can be fed through an `ocx_mirror` spec with `type: github_release`.

- **"write OCX `metadata.json` covering the env entries Homebrew would have set in its post-install script"** — accurate framing. Homebrew's `caveats` and post-install hooks set env vars (PATH, JAVA_HOME, etc.) that must be encoded as `metadata.json` `env` entries in OCX. The metadata schema supports this via `path` and `constant` modifier types (`crates/ocx_lib/src/package/metadata/env/var.rs`).

- **Prose structure: two short paragraphs** — verified. Section has exactly two paragraphs: one on the migration pattern, one on the binary-bottle case and its limitations. Compliant with `docs-style.md` "Short paragraphs" rule.

- **Reference-style link syntax used throughout** — verified. `[Homebrew][homebrew]` with definition at bottom of file. No inline `[text](url)` in body.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **Homebrew bottle layout claim: "`bin/`, `share/`, `lib/`"** — partially confirmed but incomplete. Homebrew bottles are confirmed as "simple gzipped tarballs" (Bottles docs). The Homebrew Manpage confirms the keg layout mirrors the standard Unix prefix tree (`bin/`, `share/`, `lib/`, `include/`, `libexec/`, etc.) because Homebrew installs into a prefix-structured Cellar. However, the critical nuance the text omits: Homebrew bottles have **two leading path components** before `bin/` etc. — the layout inside the tarball is `<formula>/<version>/bin/`, `<formula>/<version>/share/`, etc. (the formula definition lives at `<formula>/<version>/.brew/<formula>.rb` per the Bottles doc). This means `strip_components: 1` alone would leave a `<version>/` prefix — **`strip_components: 2`** is typically required to reach `bin/` at the root, not `strip_components: 1`. The text's example value of `1` is potentially misleading for the Homebrew bottle case specifically, even if accurate as the documented "typical case" for generic upstream archives.

- **"a public `PATH` entry"** — the phrase "public" correctly maps to `visibility: "public"` in `metadata.json` (confirmed in `crates/ocx_lib/src/package/metadata/visibility.rs:72` — `public` = both private and interface axes true). However, the section does not mention that `visibility` defaults to `"private"` (confirmed at `crates/ocx_lib/src/package/metadata/var.rs:59`), so a PATH entry without an explicit `"visibility": "public"` will be private to the package's own launchers and NOT exposed to downstream consumers. For a migration guide aimed at publishers, this default is a relevant footgun — if the publisher omits `visibility`, PATH will not propagate to consumers. This is a missing nuance rather than an error; the word "public" appears in the prose but may not clearly signal to the reader that it corresponds to an explicit JSON field value.

- **Simplification of formula fields: "formula's `url` and `sha256`"** — accurate for source formula fetching, but Homebrew binary bottles use a separate `bottle do ... root_url ... sha256 ... end` block with per-platform digests (Formula Cookbook). Publishers migrating a binary bottle (the typical case this section focuses on) should look at the `bottle do` block, not the top-level `url`/`sha256` which point to the source tarball. The simplification may send publishers to the wrong fields when dealing with binary-only Homebrew bottles.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **No inline JSON or code sample in this section** — the section is prose-only. No examples to validate. The absence of a concrete `metadata.json` snippet (showing `strip_components: 1` + `env` with `visibility: "public"`) is a style opportunity rather than an error — no block-tier issue.

### Style / convention violations [Warn]

- **"Ruby" not hyperlinked** — `docs-style.md:49` states "Every external tool mentioned must hyperlink — every occurrence, not just first." The text mentions Ruby ("Ruby DSL programs") without a link. Ruby is an external technology that warrants a link (e.g., `https://www.ruby-lang.org/`). This violates the docs-style requirement. (`migration.md:62`)

- **`strip_components: 1` as inline code, no tooltip** — `strip_components` is a technical metadata field with a non-obvious name. Per `docs-style.md` tooltip guidance, it is a good candidate for `<Tooltip term="strip_components">removes N leading path components during extraction — like tar --strip-components</Tooltip>`. The current prose does include a brief explanation ("plus a public PATH entry covers the typical case") but the term itself is left bare. Minor; consistent with the surrounding doc's style, which uses inline backtick code without tooltips for metadata fields. Not a violation, noted as opportunity.

- **"post-install script"** — Homebrew's mechanism is more precisely a formula's `caveats` method or `post_install` block (not a standalone script). The phrase "post-install script" is an informal simplification that could confuse a reader who searches Homebrew docs for that term. Low severity; does not mislead on the OCX side. Flagged as precision opportunity.

- **No `:::tip` or `:::warning` callout for the `strip_components: 2` nuance** — given the Warn finding above (that Homebrew bottles typically need `strip_components: 2`, not `1`), the guide could benefit from a `:::warning` callout. Absence is a style gap but only becomes a block if the strip_components value is confirmed wrong (see the Warn finding above).

---

## website/src/docs/authoring/migration.md#mirror — The `ocx_mirror` Pipeline

### Verified

- **`name`** top-level field — `MirrorSpec.name: String` (`spec.rs:35`). Correct.
- **`target`** top-level field with sub-fields `registry` and `repository` — `Target` struct (`spec/target.rs`), referenced as `MirrorSpec.target: Target` (`spec.rs:36`). Correct.
- **`source.type: github_release`** — `Source` enum uses `#[serde(tag = "type", rename_all = "snake_case")]`; variant `GithubRelease` serializes as `github_release` (`spec/source.rs:12-13`). Correct.
- **`source.owner`, `source.repo`, `source.tag_pattern`** — exact field names in the `GithubRelease` variant (`spec/source.rs:14-19`). `tag_pattern` has a default via `default_tag_pattern()`. Correct.
- **`assets` keyed by `<os>/<arch>` literal strings** — `AssetPatterns` deserializes from `HashMap<String, Vec<String>>` and parses keys as `Platform` (os/arch format); validated via `parse()` (`spec/assets.rs:26`). Correct shape.
- **`asset_type.type: archive`** — `UniformAssetType::Archive` with `#[serde(tag = "type", rename_all = "snake_case")]` (`spec/asset_type.rs:66-73`). Correct.
- **`asset_type.strip_components: 1`** — field `strip_components: Option<StripComponentsConfig>` on `Archive` variant (`spec/asset_type.rs:72`); integer `1` is a valid form per test `parse_asset_type_archive` (`spec.rs:700-723`). Correct.
- **`metadata.default: metadata.json`** — `MetadataConfig.default: PathBuf` (`spec/metadata_config.rs:11`); field name `default` is correct. The value `metadata.json` is plausible for the hypothetical mytool; cmake uses the same value at `mirrors/cmake/mirror.yml:35`. Correct.
- **`cascade: bool`** — top-level field on `MirrorSpec` with `#[serde(default = "default_true")]` (defaults to `true`) (`spec.rs:67`). Correct.
- **`versions.new_per_run`** — exists as `VersionsConfig.new_per_run: Option<usize>` inside the top-level `versions` block (`spec/versions_config.rs:37`). The dotted-path notation `versions.new_per_run` in the prose is correct. Confirmed used in cmake example (`mirrors/cmake/mirror.yml:48`).
- **`crates/ocx_mirror/src/pipeline/push.rs` path** — file exists at exact path (`crates/ocx_mirror/src/pipeline/push.rs`). Correct path.
- **cmake canonical example coverage** — `mirrors/cmake/mirror.yml` covers: multi-platform asset matrix (6 platforms, lines 13-28), version-range filtering (`versions.min` + `new_per_run`, lines 46-48), per-platform metadata files (`metadata.platforms`, lines 36-40). All three claims verified.
- **`[authoring-layer-reuse]` link** — resolves to `./building-pushing.md#layer-reuse`; anchor `## Reusing Layers Across Packages {#layer-reuse}` exists at `building-pushing.md:35`. Valid.
- **`[mirror-cmake]` link** — `https://github.com/ocx-sh/ocx/blob/main/mirrors/cmake/mirror.yml`; file exists at `mirrors/cmake/mirror.yml`. Valid.
- **`[in-tree-mirrors]` link** — `https://github.com/ocx-sh/ocx/tree/main/mirrors`; directory exists. Valid target.

### Inconsistent / hallucinated [Block]

- **`push.rs` described as implementing the layer-reuse pattern** — prose states the [layer-reuse pattern] "is what `crates/ocx_mirror/src/pipeline/push.rs` does in production." This is incorrect. `push.rs` implements cascade tag logic (`push_and_cascade()`) using `LayerRef::File(bundle_path)` — a fresh upload every time, no layer reuse. Layer reuse (via `LayerRef::Digest`) is the CLI-level hand-publisher pattern documented in `building-pushing.md#layer-reuse` and is not implemented in the mirror pipeline. The "does in production" attribution is a hallucination of the wrong module name. The module doing bundling is `pipeline/package.rs`; even that does not implement cross-package layer reuse — each mirror run bundles independently.  
  Source: `crates/ocx_mirror/src/pipeline/push.rs:34` (`LayerRef::File`); `crates/ocx_lib/src/publisher/layer_ref.rs` (distinction between `File` and `Digest` variants).

### Missing nuance / drift [Warn]

- **`cascade: true` shown as explicit in the minimal spec** — `cascade` defaults to `true` via `#[serde(default = "default_true")]` (`spec.rs:66-67`). Showing it explicitly in the minimal example is accurate but implies it is required; omitting it would produce the same behavior. The example could note this is the default or drop the line for true minimality. Not wrong, but potentially misleading about what is "minimal."

- **`versions.new_per_run` described as "knob"** — the field is `Option<usize>` and has no default (no cap when `None`). The prose implies it is always active ("has an explicit knob"), but it is optional; without it, the pipeline processes all unmirrored versions. Minor drift.

- **`[in-tree-mirror-spec]` link target is the `mirrors/` directory, not a spec file** — the link label `in-tree-mirror-spec` (used as first reference to `ocx_mirror`) resolves to `https://github.com/ocx-sh/ocx/tree/main/mirrors`, which is the directory tree. It shares the exact same URL as `[in-tree-mirrors]` (see refs at `migration.md:96-97`). The name implies a single canonical spec file, which does not exist (each tool has its own `mirror.yml`). Mislabeled link; creates reader confusion. Not broken, but semantically misleading.

### Broken refs [Block]

- (none) — all five reference links resolve to real targets.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **Minimal spec omits `versions` block** — the spec is technically valid without it (all fields optional), and the example is correct YAML that will parse and validate. However, for a "minimal spec wrapping a single upstream tool," omitting `versions.new_per_run` means a first run could mirror the full release history in one CI run — contrary to the doc's own stated advantage. Weak as a prescriptive example. [Warn]

- **`metadata.default: metadata.json`** — valid field + plausible value. The cmake canonical example also uses `metadata.json` as the default filename. No issue.

### Style / convention violations [Warn]

- **`[subsystem-mirror]` links to internal AI config** — resolves to `https://github.com/ocx-sh/ocx/blob/main/.claude/rules/subsystem-mirror.md`. This is an internal AI development instruction file (in `.claude/rules/`), not a user-facing schema reference. Exposing it as the authoritative "full schema" reference in public documentation is a style violation: it leaks internal AI tooling conventions into the product docs surface, and the file's content is written for AI agents, not human publishers. A proper schema reference would be the generated JSON schema at `src/public/schemas/metadata/v1.json` or a dedicated mirror spec reference page. (`migration.md:44`, `migration.md:87`, `migration.md:99`)

- **`[in-tree-mirror-spec]` and `[in-tree-mirrors]` are duplicate links** — both `[in-tree-mirror-spec]` (`migration.md:96`) and `[in-tree-mirrors]` (`migration.md:97`) resolve to the identical URL `https://github.com/ocx-sh/ocx/tree/main/mirrors`. Having two named references for the same target with different names creates implicit confusion; one should be dropped or they should be consolidated. (`migration.md:96-97`)

---

## website/src/docs/authoring/multi-platform.md#concept — One Tag, Many Manifests

### Verified

- **`ocx install mytool:1.0.0` command name is correct.** `ocx install` exists and accepts `<PACKAGES>...` positional arguments. Confirmed via `target/release/ocx install --help`.
- **`repo:tag` without registry prefix falls back to `OCX_DEFAULT_REGISTRY`.** `install.rs:33` calls `Identifier::transform_all(self.packages.clone(), context.default_registry())`, which fills in the default registry for bare `repo:tag` identifiers. Consistent with claim.
- **Merge behavior is accurate.** The function `merge_platform_into_index` in `crates/ocx_lib/src/oci/client.rs:183` merges (not replaces) a new platform into the existing index via `index.manifests.retain(|entry| entry.platform != platform)` (line 243) followed by `index.manifests.push(...)` (line 244). If the tag holds a plain `ImageManifest` (not yet an index), it wraps it into a one-entry `ImageIndex` first (lines 205–225). A missing tag starts fresh (lines 230–239). Claim "merges new platform into existing index rather than replacing" is accurate.
- **`[oci-image-index]` link target is plausible.** URL `https://github.com/opencontainers/image-spec/blob/main/image-index.md` is the canonical OCI Image Index spec location as also cited in `docs-style.md:51`. Not fetched live (read-only constraint), but matches the form endorsed by the project's own docs style rule.
- **`[cmd-package-push]` anchor exists.** `website/src/docs/reference/command-line.md:830` contains `#### \`push\` {#package-push}`. Link target `../reference/command-line.md#package-push` resolves correctly.
- **"Manifest of manifests" concept frame is accurate.** An OCI Image Index is structurally a list of manifest descriptors (one per platform), making the phrase precise.
- **Narrative structure opens with concept frame.** The section opens with a single-sentence definition ("An OCI Image Index is a manifest of manifests"), consistent with `docs-style.md` §"Narrative Structure" requirement for an idea sentence first.

### Inconsistent / hallucinated [Block]

- **`crates/ocx_lib/src/oci/manifest_index.rs` does not exist.** No file at this path or any path containing `manifest_index` exists under `crates/ocx_lib/src/oci/`. The actual image-index assembly logic (`merge_platform_into_index`, `push_manifest_and_merge_tags`) lives in `crates/ocx_lib/src/oci/client.rs` (lines 173–260 and 447–500). The doc cites a fabricated file path. **Block: the internal source path is wrong and will mislead any publisher or contributor who looks for the code.**

### Missing nuance / drift [Warn]

- **`ocx package push` trigger for merge.** The doc states "when `ocx package push` sees a tag that already has a manifest, it merges…" This is true for the default (non-`--new`) path, but elides the `--new` flag which creates a fresh tag even if one already exists (confirmed from `package_push.rs:62` flow and command-line.md). A publisher reading only the concept section could be confused if they accidentally push without `--new` to an unrelated existing tag. The `#pattern` section does cover this distinction; the `#concept` section's omission is a minor nuance gap, not a factual error. Warn level.

### Broken refs [Block]

- **Internal source path `crates/ocx_lib/src/oci/manifest_index.rs` is broken** — file does not exist. Correct path is `crates/ocx_lib/src/oci/client.rs` (function `merge_platform_into_index` at line 183). See "Inconsistent / hallucinated" above.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none)

### Style / convention violations [Warn]

- **Internal source file path exposed in publisher-facing doc.** The sentence "The image-index assembly logic lives in `crates/ocx_lib/src/oci/manifest_index.rs`" leaks an internal implementation path into a user-facing authoring guide. `docs-style.md` does not explicitly prohibit this pattern, but it contradicts the principle of keeping docs focused on user goals rather than internal structure. Even if the path were correct, it would be borderline for a publisher-facing page whose audience needs to know the behavior, not the file layout. **Warn: style — internal path leaked into user-facing doc; independently moot because the path is also wrong (see Block above).**

---

## website/src/docs/authoring/multi-platform.md#metadata — Use the Same Metadata Across Platforms

### Verified

- **`ocx_mirror` spec accepts per-platform metadata override.**
  `MetadataConfig` struct (`crates/ocx_mirror/src/spec/metadata_config.rs:10-13`) has `default: PathBuf` and `platforms: HashMap<String, PathBuf>`. `resolve_metadata()` (`crates/ocx_mirror/src/pipeline/package.rs:80-94`) picks the platform-specific file when present, falls back to `default`. Confirmed.

- **`metadata.platforms` block exists in `mirrors/cmake/mirror.yml`.**
  Lines 34-40 of `mirrors/cmake/mirror.yml` contain a `metadata:` key with a nested `platforms:` sub-key mapping `darwin/amd64`, `darwin/arm64`, `windows/amd64`, `windows/arm64` to separate JSON files. The doc's phrasing "`metadata.platforms` block" is informal but unambiguous as a dotted-path reference. Confirmed.

- **`--metadata <path>` per push for hand-driven publishers.**
  `PackagePush` (`crates/ocx_cli/src/command/package_push.rs:29-30`) declares `#[clap(short, long)] metadata: Option<std::path::PathBuf>`. Each invocation is independent; calling `ocx package push` multiple times with different `--metadata` paths is fully supported. Confirmed.

- **Default behavior: single `metadata.json` covers every platform.**
  For `ocx package push`: when `--metadata` is omitted, `infer_metadata_file()` (`crates/ocx_cli/src/conventions.rs:10-27`) derives a single sidecar path from the archive filename — that same sidecar applies regardless of `--platform`. For `ocx_mirror`: the `metadata.default` value applies to any platform without an explicit entry in `metadata.platforms`. Both paths confirm the "one file covers all platforms" default. Confirmed.

- **Examples mentioned in prose (Windows env keys, different entry-point names) are plausible divergence reasons.**
  No assertion to verify in source code; these are illustrative examples. No issue.

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **Link text calls `mirrors/` the `ocx_mirror` "spec".**
  Line 44: "`the [ocx_mirror][in-tree-mirror-spec] spec accepts a per-platform metadata override`" — `[in-tree-mirror-spec]` resolves to `https://github.com/ocx-sh/ocx/tree/main/mirrors` (line 61), which is the *collection of in-tree mirror instances*, not the spec format definition. The `MirrorSpec` YAML format is defined in `crates/ocx_mirror/src/spec/`. Linking `mirrors/` as "the spec" conflates spec instances with the spec schema. Not a hard error (the examples demonstrate the feature), but a reader following the link sees concrete YAML files, not format documentation. Consider pointing to `crates/ocx_mirror/` or the reference docs if a mirror-spec reference page exists.

### Broken refs [Block]

- (none)

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none) — No inline code examples in this section.

### Style / convention violations [Warn]

- **`mirrors/cmake/mirror.yml` mentioned without a link (line 44).**
  The file already defines `[mirror-cmake]: https://github.com/ocx-sh/ocx/blob/main/mirrors/cmake/mirror.yml` (line 60) and uses it on line 18. Line 44 refers to the same file as plain text (`` `mirrors/cmake/mirror.yml` ``) instead of `[`mirrors/cmake/mirror.yml`][mirror-cmake]`. Per `docs-style.md`: "Every reference to another part of system must hyperlink." This is a Warn: the link definition exists but is unused at this mention.

- **"per-platform metadata override" — borderline Tooltip candidate.**
  `docs-style.md` suggests tooltips for jargon that interrupts prose flow. This term is explained immediately in context ("when platforms genuinely diverge — say, Windows needs different env keys…"), so no tooltip is needed. Acceptable as-is; no change required.

---

## website/src/docs/authoring/multi-platform.md#pattern — The Per-Platform Push Pattern

### Verified

- **`-i` flag for `--identifier`**: correct. `package_create.rs:16` declares `#[clap(short, long)]` on the `identifier` field; clap derives `-i`.
- **`-p` flag for `--platform`**: correct. `package_create.rs:19` same pattern; clap derives `-p`.
- **`-o .` triggers directory-mode inferred filename**: correct. `package_create.rs:41–51` — if `-o` target is an existing directory, the filename is inferred via `infer_filename()`; if omitted entirely, the inferred name is used directly.
- **Inferred archive name format**: `<name>-<tag>-<os>-<arch>.tar.xz` — correct. `infer_filename()` at `package_create.rs:90–104` does `format!("{}-{}", identifier.name(), identifier.tag_or_latest())` then appends `format!("-{}", platform.ascii_segments().join("-"))` then `.tar.xz`. For `mytool:1.0.0` on `linux/amd64` this yields `mytool-1.0.0-linux-amd64.tar.xz`, matching the code fence.
- **Sidecar copy behaviour**: correct. `package_create.rs:79–84` calls `infer_metadata_file(&output)` (from `conventions.rs:11–27`) which strips the archive extension and appends `-metadata.json`. So `mytool-1.0.0-linux-amd64.tar.xz` yields `mytool-1.0.0-linux-amd64-metadata.json`.
- **`-n` is the short flag for `--new` in `ocx package push`**: correct. `package_push.rs:23` `#[clap(long = "new", short = 'n')]`.
- **Step 2 claim "Push reads sidecar next to the layer"**: correct. `package_push.rs:64–76` — when `--metadata` is absent, it calls `infer_metadata_file(first_file_layer)` to locate the sidecar.
- **Step 3 claim "OCX merges new platform into image index"**: correct. `client.rs:457–486` — `push_manifest_and_merge_tags` calls `merge_platform_into_index`, which fetches the existing index (or creates one), upserts the new platform entry, and re-pushes.
- **`mirrors/cmake/mirror.yml` exists**: confirmed at `mirrors/cmake/mirror.yml`. It is a multi-platform spec (six platform entries: linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64, windows/arm64) that matches the described pattern.
- **Cast file exists**: `website/src/public/casts/package-multi-platform.cast` present.
- **`ocx index update` command exists**: `command/index_update.rs` exists; anchor `#index-update` at `reference/command-line.md:422`.
- **`ocx install` command exists**: `command/install.rs` exists; anchor `#install` at `reference/command-line.md:453`.
- **"Candidate symlink" term**: correct OCX terminology. `arch-principles.md` defines "Candidate — Symlink at `symlinks/{registry}/{repo}/candidates/{tag}` — pinned at install time". `storage.md:201` uses "candidates/{tag} — pinned to a specific version. Created by `ocx install`". The phrase "binary on the candidate symlink" correctly describes what `ocx install` produces.
- **All six link targets resolve**:
  - `[mirror-cmake]` → `https://github.com/ocx-sh/ocx/blob/main/mirrors/cmake/mirror.yml` (valid external URL to existing file)
  - `[cmd-package-create]` → `command-line.md#package-create` (anchor at line 773)
  - `[cmd-package-push]` → `command-line.md#package-push` (anchor at line 830)
  - `[cmd-index-update]` → `command-line.md#index-update` (anchor at line 422)
  - `[cmd-install]` → `command-line.md#install` (anchor at line 453)
  - `[authoring-bundle-sidecars]` → `bundle-anatomy.md#sidecars` (anchor at line 44)
- **Narrative "After the third push, `mytool:1.0.0` resolves to an index manifest with three platform descriptors"**: internally consistent with the doc's own three-platform code fence — the claim is about the three pushes shown in the fence, not about the cast.
- **Style structure**: numbered procedure → code fence → narrative recap → cast — strong and consistent with docs-style.md conventions.

### Inconsistent / hallucinated [Block]

- **`<repo>` placeholder vs `identifier.name()` semantics**: The doc (step 1) writes the inferred name pattern as "`<repo>-<tag>-<os>-<arch>.tar.xz`". The actual format uses `identifier.name()` (`package_create.rs:92`), which returns the **last path segment** of the repository (e.g., for `myorg/cmake`, `name()` returns `cmake`, not `myorg/cmake`). The placeholder `<repo>` is ambiguous — readers with namespaced repos (e.g., `ocx.sh/tools/mytool:1.0.0`) will expect `tools/mytool` but get `mytool`. The `bundle__sidecars` agent already flagged this as Block; confirmed identical defect here. The correct placeholder is `<name>` (matching `identifier.name()`, i.e., the final path segment).

  Source: `crates/ocx_cli/src/command/package_create.rs:92`, `crates/ocx_lib/src/oci/identifier.rs:142–147`.

- **Cast title vs. cast and doc content mismatch**: The `<Terminal>` component attribute says `title="Pushing two platforms and verifying the index"`, but the doc's own prose code fence shows three platforms (`linux/amd64`, `linux/arm64`, `darwin/arm64`). The cast itself pushes exactly **two platforms** (`linux/amd64`, `linux/arm64`). Two independent problems:
  1. The code fence shows 3 platforms; the cast shows 2 — they differ.
  2. The prose narrative immediately after the code fence says "after the **third** push" — this is correct for the code fence (3 platforms) but wrong for the cast (only 2 platforms pushed).
  The title "two platforms" matches the cast but not the code fence. The narrative "after the third push" matches the code fence but not the cast.

  Source: `website/src/public/casts/package-multi-platform.cast` (commands extracted); `test/recordings/scripts/package-multi-platform.sh` (2 creates, 2 pushes); `website/src/docs/authoring/multi-platform.md:29–36` (3-platform code fence).

- **Code fence omits `-c`/`--cascade` flag, but cast includes it**: The doc code fence shows `ocx package push -n -p linux/amd64 ...` and `ocx package push -p linux/arm64 ...` — no cascade flag. The actual cast (and recording script) uses `ocx package push -n -c -p linux/amd64 ...` and `ocx package push -c -p linux/arm64 ...`. Without `-c`, rolling tags (`1`, `1.0`, `latest`) are **not** updated; only the exact `mytool:1.0.0` tag is created. The code fence silently drops the cascade behavior without explanation, making the pattern incomplete for real-world publish workflows that need rolling tags.

  Source: `test/recordings/scripts/package-multi-platform.sh:6–7`; `crates/ocx_cli/src/command/package_push.rs:17–19`.

- **`package create` path argument differs between code fence and cast**: The doc code fence uses three separate source directories (`build-amd64`, `build-arm64`, `build-darwin`). The cast recording script uses `build` for both creates. This is not a flag error — the positional `path` argument differs. The cast approach bundles the same `build/` directory twice (once for each platform), while the doc implies three separate per-platform build outputs. Neither is wrong in isolation, but they describe different workflows and cannot both be "the pattern" for the same recording.

  Source: `test/recordings/scripts/package-multi-platform.sh:4–5`; `website/src/docs/authoring/multi-platform.md:23–25`.

### Missing nuance / drift [Warn]

- **Narrative says the cast "runs `ocx index update` and `ocx install` after the second push"**: The cast does run `ocx index update mytool` after the second push, then `ocx index list mytool --platforms`, then `ocx install mytool:1.0.0`, then `ocx exec mytool:1.0.0 -- mytool`. The narrative omits `ocx index list` and `ocx exec`, which are also in the cast and contribute to the "consumer side" demo. Minor omission; not incorrect.

  Source: cast command sequence extracted from `website/src/public/casts/package-multi-platform.cast`.

- **"Every in-tree mirror uses [this flow]"**: The cmake mirror uses `ocx_mirror` tooling (automated), not the manual `ocx package create / ocx package push` shell pattern described in this section. The mirror YAML drives a different code path (`ocx_mirror` binary with `mirror.yml` spec). The cmake mirror is a valid "worked spec" for the multi-platform metadata layout, but it is not an example of manually running `ocx package create` + `ocx package push`. The link is still useful, but the framing "every in-tree mirror uses [this flow]" slightly overstates the correspondence.

  Source: `mirrors/cmake/mirror.yml` (mirror spec, not shell commands); `crates/ocx_mirror/` (separate binary).

- **Step 1: `-m metadata.json -o .` order**: The code fence passes `-m` before `-o`. The code correctly handles both flags independently, so order does not matter functionally. No correctness issue, but the example `-o .` placing at the end is consistent with clap flag ordering conventions.

### Broken refs [Block]

- (none) — all six link definitions resolve to existing anchors or valid external URLs.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- **[Block] Code fence shows 3 pushes; cast records 2 pushes**: The code fence (`linux/amd64`, `linux/arm64`, `darwin/arm64`) and the cast (`linux/amd64`, `linux/arm64`) are out of sync. A reader using the cast as the animated companion to the code fence will see mismatched platform coverage. The `<Terminal>` `title` attribute ("Pushing two platforms") correctly describes the cast but contradicts the code fence.

  Source: `website/src/public/casts/package-multi-platform.cast`; `website/src/docs/authoring/multi-platform.md:23–36`.

- **[Block] Code fence omits `-c` flag on push commands**: The cast recording — the ground-truth source — uses `ocx package push -n -c ...` and `ocx package push -c ...`. The doc code fence drops `-c` silently. Readers following the code fence literally will not get cascade/rolling-tag updates. This is either a documentation error (should add `-c` to code fence) or an intentional simplification that must be explained inline.

  Source: `test/recordings/scripts/package-multi-platform.sh:6–7`; `crates/ocx_cli/src/command/package_push.rs:17–19`.

- **[Warn] Cast title string mismatch with `<Terminal>` component attribute**: `title="Pushing two platforms and verifying the index"` (component attribute) vs. cast header `"title": "Publishing a multi-platform package"` (`.cast` JSON). The `<Terminal>` component `title` prop overrides the cast's internal title for display, so the rendered page will show "Pushing two platforms…" regardless — but the mismatch makes the cast's own header stale and misleading to anyone reading the raw file.

  Source: `website/src/public/casts/package-multi-platform.cast` line 1 (JSON header); `website/src/docs/authoring/multi-platform.md:37` (`<Terminal>` tag).

### Style / convention violations [Warn]

- (none) — numbered procedure → code fence → narrative → cast follows `docs-style.md` conventions. Reference-style links used throughout, link definitions collected at bottom of file. Heading has custom `{#pattern}` anchor. Paragraphs are short and each conveys one idea.

---

## website/src/docs/authoring/multi-platform.md#stability — The Image Index Is Stable

### Verified

- **"Each per-platform manifest's digest depends only on its own bytes."**
  Verified. `OciImageManifest` is serialized via `serde_json::to_vec` from a deterministic struct: fields are in a fixed declaration order, layers are a `Vec` in caller-supplied order (content-hash stable), annotations use `BTreeMap` (sorted keys). No non-deterministic fields.
  Source: `external/rust-oci-client/src/manifest.rs:75–129`, `crates/ocx_lib/src/oci/client.rs:638–653`.

- **"Push the same archive twice and the manifest digest is identical"**
  Verified. At push time, OCX computes the layer digest from the archive bytes (`Algorithm::Sha256.hash_file_read`), then the config digest from the metadata JSON, then serializes the manifest struct. No timestamp annotation or random field is injected. `annotations: None` throughout. The manifest is a pure function of (archive bytes × metadata bytes × platform). Same archive → same layer digest → same config digest → same manifest JSON → same manifest digest.
  Source: `crates/ocx_lib/src/oci/client.rs:512–663`. Confirmed `annotations: None` at line 649; no `CREATED` annotation added. `annotations.rs:10` defines the key but it is never set during push.

- **"Index manifest's digest is a function of its descriptors"**
  Verified. `merge_platform_into_index` builds `OciImageIndex { ..., annotations: None }`, serializes with `serde_json::to_vec`, hashes to produce `index_digest`. The index structure is deterministic: `manifests` is a `Vec` appended in a consistent order (existing entries retained via `retain`, new entry pushed last), no timestamp fields.
  Source: `crates/ocx_lib/src/oci/client.rs:183–258`.

- **"It changes when (and only when) you add a platform or push a new build for an existing platform"**
  Verified. `index.manifests.retain(|entry| entry.platform != platform)` removes the existing entry for the platform, then pushes the new one. A same-platform push with the same manifest digest results in the same descriptor list and the same index digest. An idempotent re-push (same bytes, same platform) produces the same index.
  Source: `crates/ocx_lib/src/oci/client.rs:243–250`.

- **`[authoring-building-pushing-cascade]` link target exists**
  Verified. Reference resolves to `./building-pushing.md#cascade` (`multi-platform.md:75`). The `## Cascading Rolling Tags {#cascade}` heading exists at `website/src/docs/authoring/building-pushing.md:27`.

- **"Every layer caches independently"**
  Verified as an accurate description of OCI layer deduplication. Layers are content-addressed blobs stored by digest in the registry. The storage.md confirms the local layer store (`~/.ocx/layers/`) is also digest-keyed and shared across packages (`storage.md:20,113`).

### Inconsistent / hallucinated [Block]

- (none)

### Missing nuance / drift [Warn]

- **Non-deterministic archive digests not flagged in #stability section.** `building-pushing.md:23` (same file group) explicitly states bundles are NOT byte-reproducible across `ocx package create` runs ("timestamps, compression entropy"). The claim "push the same archive twice" is only true if the *same on-disk bytes* are re-pushed. The `#stability` section does not clarify this. A reader may infer that re-running `ocx package create` and re-pushing would yield the same manifest digest — it would not. The stability guarantee holds at the byte level; it does not guarantee stability across separate bundle runs. Low severity (the sibling page states the caveat), but the #stability section could be read in isolation.
  Related: `website/src/docs/authoring/building-pushing.md:23`.

### Broken refs [Block]

- (none in `#stability` section)

  **Note (out-of-scope, in `#concept` section line 14):** `multi-platform.md:14` references `crates/ocx_lib/src/oci/manifest_index.rs` — this file does not exist. The merge logic lives in `crates/ocx_lib/src/oci/client.rs` (`merge_platform_into_index`). This is outside the `#stability` anchor but is a factual inaccuracy in the same file.

### Example/cast/JSON sample issues [Block if invalid, Warn if weak]

- (none — no code samples or JSON in the #stability section)

### Style / convention violations [Warn]

- (none — prose-only section, no link syntax violations, no inline URLs)

---

