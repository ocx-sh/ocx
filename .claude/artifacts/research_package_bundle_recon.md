# Research: Package export/import and Bazel hermeticity — codebase recon

## Metadata

**Date:** 2026-11-08
**Domain:** packaging
**Triggered by:** `.agents/discussions/bazel-hermetic-package-bundles.md` (hex-discuss entry wave, codebase lane)
**Expires:** 2027-02-08 (code moves fast; re-verify file:line before citing)

## Direct Answer

rules_ocx keeps package content in the shared, absolute `$OCX_HOME` outside Bazel and bakes absolute paths into eager launchers; remote execution is a documented non-goal. ocx has no OCI image-layout reader or writer, and `ocx package copy` writes only to registries. Offline/frozen modes and a per-invocation `OCX_HOME` exist.

## rules_ocx materialization (`/home/mherwig/dev/rules_ocx`)

- Store is the shared `$OCX_HOME` (default `~/.ocx`), not the output base. `docs/concepts/how-it-works.md:19,24-26`: repo rules run unsandboxed; "Launchers therefore reference absolute store paths, as nixpkgs does. Remote execution is a non-goal at 0.5.0."
- External repo holds only symlinks into the store: `ocx/private/package.bzl:255-257` (`ctx.symlink(root + "/content", "content")`, `root` from `ocx package which`); fails when `root` is not absolute (`package.bzl:223-227`).
- `OCX_HOME` resolution: `repo_utils.bzl:679-692` (`ctx.getenv("OCX_HOME")`, else `$HOME`/`USERPROFILE`), must be absolute.
- `isolated_home` puts the store inside the repo (`repo_utils.bzl:327`, `package.bzl:333-340`); incompatible with `bins` (`package.bzl:60-62`) because a lazy launcher "must resolve the store on whatever machine executes it".
- Absolute paths in launchers: `render_launcher` (`repo_utils.bzl:1440-1549`) writes `export OCX_HOME=<abs>`, `export OCX_BINARY_PIN=<abs ocx>`, and the exec target's absolute path; called from `package.bzl:283`. `env.bzl` exposes `OCX_HOME` likewise (`repo_utils.bzl:1592`).
- Lazy mode (`bins`): text-only launchers running `ocx package exec '<pkg@sha256:...>' -- <name>` via `rlocation` of the ocx binary — relative on POSIX, absolute on Windows (`package.bzl:51-101`, `repo_utils.bzl:1247`). Fetch touches neither network nor ocx (`package.bzl:54`); content "never a Bazel input" (`package.bzl:57-58`).
- Digest flow: `ocx.package` `pins` (platform → manifest digest), `pinned_ref` rewrites to `repo@digest` (`package.bzl:103-119`); lazy refuses non-digest refs (`package.bzl:66-69`). `ocx.project` watches committed `ocx.lock` (`extensions.bzl:87-90`). ocx CLI download sha256-verified from vendored `dist/dist.json` (`how-it-works.md:17`). `index` attr → `--index <dir> --frozen` (`package.bzl:166-169`). Nothing but the ocx binary is sha256-pinned at the Bazel layer; OCI digests are strings handed to `ocx`.
- Network phases: module extension none (`reproducible = True`, `extensions.bzl:331`); `@ocx_tool` download yes (`download.bzl:36,44`); eager repo rule yes via `ocx package install`/`pull`, `retries = 2` (`package.bzl:186-195`); lazy mode pulls at first action execution (`repo_utils.bzl:115-120` warns eager launchers must not download inside an action).
- Env forwarding: `OCX_OFFLINE`, `OCX_FROZEN`, `OCX_MIRRORS`, `OCX_INDEX` forwarded as `site` rows (`repo_utils.bzl:50-65`); `OCX_NO_VERIFY`, `OCX_ALLOW_YANKED` explicit-only (`repo_utils.bzl:75-76`).
- No `toolchain_type` / `register_toolchains` found (grep over `.bzl/.bazel/.yml/.md/.sh`). Platform selection is a `select()` hub over `config_setting`s (`package.bzl:367-406`); `examples/cross_platform/transition.bzl` exists.
- "Offline" proof: `taskfile.yml:163-185` `deep:offline-version` (nightly, Linux): fetch with network → `OCX_OFFLINE=1 bazel fetch --force` against warm `OCX_HOME` → same with empty `OCX_HOME` must fail. Proves warm-store sufficiency and fail-closed; not bundle-built stores, not sandboxed actions.
- Remote cache: `bazel/baseline.bazelrc:3-9`, uploads off by default.

## ocx export/import building blocks (`/home/mherwig/dev/ocx`)

- No OCI image-layout code: no hits for `image.layout|oci-layout|ImageLayout` in `crates/**` or `external/**/*.rs`. `ocx_util::archive` = tar, zip, backend (`crates/ocx_util/src/archive/{tar,zip,backend,error,extract_options}.rs`), extraction-oriented.
- `package copy` resolves `--to`/`--identifier` as registry refs only (`crates/ocx_cli/.../package_copy.rs:17-25,93-94`).
- Offline/frozen: `OCX_OFFLINE`/`--offline` (`crates/ocx_env/src/vars.rs:29-37`; tags from local index or digest pin); `OCX_FROZEN` (`vars.rs:47`, offline wins, `:44-46`); `--offline` verify fails closed 81 (`options/verify.rs:44`).
- `OCX_HOME` public env var, per-invocation override (`vars.rs:20-28`); `--index`/`OCX_INDEX` separately (`context.rs:307-308`).
- Relative vs absolute (partial): `ocx_util::fs::symlink::validate_target` refuses absolute targets (`symlink.rs:12-21,425-428`); toolchain `active` link relative on POSIX, absolute on Windows junctions (`toolchain_store.rs:35-43`); hardlinks make `$OCX_HOME` relocatable as a unit on one volume (`adr_three_tier_cas_storage.md:92,110,224`), cross-volume `CrossesDevices` (`hardlink.rs:6`); `projects/` GC ledger links target absolute project dirs (`subsystem-file-structure.md` ARCH-4b). `refs/*` and `symlinks/` target form **unconfirmed**.

## Prior design artifacts (`.claude/artifacts/`)

- `adr_three_tier_cas_storage.md` — three tiers; single-volume relocatable home; OCI Image Layout surveyed (`:51,497`); "ocx export" mention at `:561`.
- `adr_offline_verify_trust_cache.md` — air-gapped verify; true no-registry air-gap is #99's concern (`:39`); bundle-local-CAS air-gap "honestly deferred" (`:103`).
- `research_servable_index_snapshot.md:119-121`, `design_spec_servable_index_snapshot.md:852,992`, `adr_servable_index_snapshot.md:535` — cite image-layout / ORAS tarball as air-gap transport references.
- `research_content_addressed_storage.md`, `research_oci_digest_cache_domain.md`, `research_oci_metadata_cache_tech.md` — survey material.
- `adr_bazel_build_adoption.md:155,1530,2911` — "no remote execution, ever, under this ADR" (ocx's own build, not rules_ocx).
- `adr_oci_registry_mirror.md:85,327`, `adr_index_indirection.md`, `adr_managed_config_tier.md:186,359` — mirror/index indirection as the existing offline-ish path.
- `adr_shim_hermetic_zigbuild.md` — "hermetic" = reproducible shim build, unrelated.

## negative

- No `ocx export`/`ocx import`, no image-layout writer/reader, no non-registry copy sink, no `toolchain_type` in rules_ocx, no sandboxed/network-isolated proof in rules_ocx. No ADR dedicated to export/import (filenames only).

## leads

- `adr_three_tier_cas_storage.md:561` and issue #99 / #23 / #25 text — true scope of "relocatable".
- `ocx_store` ref/symlink creation — relative or absolute targets.
- `adr_servable_index_snapshot.md` — may already define a portable index bundle export could reuse.
- `ocx_package_manager` launcher generation (entrypoints, `OCX_BINARY_PIN`) — absolute paths decide relocatability.
- rules_ocx `docs/guides/offline-mirror`, `docs/concepts/trust-and-config.md` — current documented air-gap story.
