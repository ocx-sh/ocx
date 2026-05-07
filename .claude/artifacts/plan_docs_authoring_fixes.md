# Authoring Docs — Fact-Check Fix Plan & Application Log (2026-05-07)

## Source

Aggregated findings: `.claude/artifacts/docs_authoring_factcheck_2026-05-07.md` (2363 lines, 35 per-subsection Sonnet fact-check reports).

35 agents, one per H2/H3 subsection across the 8 new `website/src/docs/authoring/*.md` pages, fact-checked prose against:

- `crates/ocx_lib/src/**`, `crates/ocx_cli/src/**`, `crates/ocx_mirror/src/**`
- `./target/release/ocx <subcmd> --help` for every CLI command referenced
- `website/src/public/schemas/metadata/v1.json` for every JSON sample
- `website/src/public/casts/*.cast` for every embedded recording
- `.claude/rules/docs-style.md` and `.claude/rules/subsystem-website.md` for component / link / callout compliance

## Block-tier fixes applied

### `index.md`

- `#tldr`: "next to the extracted layer" → "as a sibling of `content/` in the assembled package directory" (architecture: layer store and package store are distinct tiers).

### `bundle-anatomy.md`

- `#what-goes-in`: removed "verification anchor" claim (no tool-level guarantee); rewrote `content/` paragraph to clarify it is the post-install OCX directory, not a publisher-side archive layout requirement; removed false "every in-tree mirror follows" claim.
- `#sidecars`: pattern `<repo>-<tag>-<os>-<arch>` → `<name>-<tag>-<os>-<arch>` (`<name>` = last segment of OCI repository); added codec-inference clarification (only `.tar.xz` when `-o .` is used); dropped `<code>metadata.json</code>` inside `<Description>` (Tree component silently strips inline `<code>` from descriptions).
- `#strip-components`: hyperlinked `tar --strip-components` → GNU tar manual; documented default of `0`.

### `dependencies.md`

- `#when`: corrected "80 MB tarball" → "~30 MB tar.xz / ~57 MB Gzip"; switched bare `nodejs:24` to `ocx.sh/nodejs:24`; hyperlinked Node.js, npm, terraform, cmake.
- `#name-field`: replaced invalid `myorg/cmake@sha256:...` identifiers with `ocx.sh/myorg/cmake@sha256:...` (parser rejects bare org/repo without explicit registry); clarified that override values must satisfy slug pattern; reframed default-derivation as "last path segment".
- `#edge-visibility`: corrected `resolve.json` pointer (lives in `in-depth/environments.md`, not `in-depth/dependencies.md`).
- `#ordering`: clarified topological-order preservation for transitive deps and that the importing package emits last.

### `entry-points.md`

- `#why`: removed false "strips ambient env" claim — launcher inherits ambient and overlays package private-surface env on top; tightened "process tree isolation" framing into "neither launcher exposes its pinned runtime as a bare PATH entry"; hyperlinked Node.js.
- `#when`: removed false "substitution happens at install time and bakes into launcher" claim — replaced with publish-time validation + exec-time resolution model; corrected "Cmake" → "CMake"; hyperlinked Python, Node.js, JVM, Ruby, Go, Rust, CMake, ripgrep, mold; clarified synth-PATH path as `entrypoints/` directory shown to consumers as `<symlink-root>/current/entrypoints`.
- `#naming`: corrected "OCX detects collision at select time" — collision detection runs at install time and at compose time, never at select; replaced "multi-owner error" (not a real source-code term) with the actual `EntrypointCollision` variant; hyperlinked CMake.
- `#target`: replaced "OCX rejects launchers whose target is missing" with the accurate publish-time-validation + install-time-dep-root + exec-time-binary-resolution model; added `:::warning` callout flagging that target binary is not stat'd at install.
- `#python-example`: corrected `python:3.13` → `cpython:3.13` (the OCX repository is `ocx.sh/cpython`); fixed PATHEXT cross-reference (lives in `command-line.md`, not `in-depth/entry-points.md`); converted second `:::tip` (background) to `:::info`; renamed anchor `#python-example` → `#target-python-example` to match parent-subsection convention; hyperlinked Python, mise, Go, Rust.

### `env-surface.md`

- `#templates`: corrected "at install time" → "at exec time" (env templates resolved by `ocx exec`/`ocx env`, never persisted); added concrete JSON code sample; clarified that validation fires both during `ocx package create --metadata` and `ocx package push`.
- `#visibility`: corrected `ocx exec mypkg` → `ocx exec mypkg -- <cmd>` (clap requires the `--` terminator); hyperlinked Java, cmake, node, uv.
- `#last-wins`: removed unconditional "diagnostic" claim — silenced overwrite is the default in `ocx exec`/`ocx env`; only `ocx shell profile load` and `ocx ci export` emit the warning; hyperlinked Java.
- `#migrating`: "feature port" → "feature release"; hyperlinked Java, CMake; clarified synth-PATH path.

### `building-pushing.md`

- `#first-push`: corrected `--new` framing — flag is a `--cascade` modifier, not a standalone "first push" optimization; the example command no longer carries `-n` outside cascade.
- `#byo-archives`: removed "referrer-only manifests / signature attestations" claim (no Referrers API, no `subject` field implemented); replaced with accurate `__ocx.desc` description-tag explanation; converted to `:::warning` callout.
- `#layer-reuse`: broadened digest syntax to `<algo>:<hex>.<ext>` accepting sha256/sha384/sha512; corrected pathological-filenames callout (a bare `<algo>:<hex>` without extension is *rejected*, not parsed as digest); softened the "every in-tree mirror runs this pattern" claim — `ocx_mirror` always uploads file layers, the digest-reuse pattern is hand-driven only.
- `#cascade`: collapsed duplicate `[in-depth-versioning-cascades]` reference; added per-platform cascade nuance.

### `multi-platform.md`

- `#concept`: removed fabricated source path `crates/ocx_lib/src/oci/manifest_index.rs` (actual logic in `client.rs`); removed internal source path leak from publisher-facing doc; added `--new`-as-cascade-modifier note.
- `#pattern`: rewrote per-platform push sequence to match the recorded cast (two platforms, shared `build` source dir, `--cascade` on every push); replaced `<repo>` with `<name>`; added cross-link to `mirror-pipeline`.
- `#metadata`: hyperlinked `mirrors/cmake/mirror.yml`.
- `#stability`: clarified "same archive bytes" qualifier and pointed at `bundle-anatomy.md#stable` for re-run-non-determinism.

### `migration.md`

- `#mirror`: corrected "shells out to `ocx package …`" — `ocx_mirror` calls the `ocx_lib` publisher API directly; corrected layer-reuse attribution (mirror always uploads file layers); clarified `versions.new_per_run` as `Option<usize>`; documented `cascade` default of `true`; removed `[subsystem-mirror]` link to internal AI config from public See Also.
- `#github-releases`: corrected pattern-selection mechanism (union must resolve to exactly one filename per release; multiple matches produce `Ambiguous`); hyperlinked CMake, curl.
- `#homebrew`: corrected `strip_components: 1` → `strip_components: 2` for binary bottles (`<formula>/<version>/bin/...` layout); flagged the `visibility: "public"` requirement (default is `private`); hyperlinked Ruby; clarified bottle vs. source-tarball distinction.
- `#describe`: removed "referrer manifest" claim — descriptions land under the dedicated internal `__ocx.desc` tag, not via OCI Referrers API; removed false "`ocx_mirror` runs `package describe` automatically" claim; added `--logo` / `--keywords` to the example.

### Adjacent fix: `in-depth/environments.md`

- Repaired broken anchor: `[ug-conflicts]: ../user-guide.md#dependencies-environment` → `…#conflict-warnings` (the user-guide subsection's auto-generated anchor).

## Verification

`task website:build` ran cleanly after all edits — schema regen, recordings (22 passed), SBOM (272 components), catalog (15 packages), and VitePress 2.0 build succeeded with no dead links.

## Deferred Warn-tier (not auto-applied)

These were flagged by the fact-check fleet but kept as-is:

- Borderline tooltip-vs-prose calls for "verification anchor"-style jargon (no longer relevant after Block fix).
- `package-create.cast` is a thin recording for `bundle-anatomy.md#stable` — could be re-recorded to show the digest-capture flow, but the cast file is shared infrastructure and out of scope for a doc edit.
- `package-push.cast` and `package-multi-platform.cast` internal `title` fields differ from the rendered `<Terminal title="…">` prop. Component prop wins at render time; cast metadata cleanup is a separate housekeeping pass.
- `package-multi-platform.cast` records two platforms, the rewritten code fence now matches; deeper reconciliation (re-record with three platforms or simplify the cast title) deferred.
- LZMA compression-non-determinism completeness gap in `bundle-anatomy.md#stable` — minor.

## Per-finding-file detail

See `.claude/artifacts/factcheck/<file>__<anchor>.md` (35 files) for the full per-subsection report each Sonnet agent produced, including verified-clean claims that did not require any change.
