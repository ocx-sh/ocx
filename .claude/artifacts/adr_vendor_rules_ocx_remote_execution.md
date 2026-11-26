# ADR: `ocx vendor`, its manifest and a native launcher, consumed by rules_ocx under remote execution

## Metadata

**Status:** Proposed
**Date:** 2026-11-26
**Deciders:** architect worker of a `/hex-architect` run (tier high), inside an unattended goal loop; the owner rules on the result
**Beads Issue:** N/A
**Related PRD:** `.agents/discussions/bazel-rbe-rules-ocx.md` (ratified 2026-11-26, the "source" below)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, Tokio, no new language or runtime)
- [x] Deviations justified below (DV-1 … DV-11)
**Domain Tags:** api | integration | infrastructure | security | devops
**Supersedes:** the Bazel decisions of `.agents/discussions/bazel-hermetic-package-bundles.md` that the source reverses (extraction in a build action, one repository per package, the repository rule running `ocx package export`)
**Superseded By:** —
**Companion:** [`system_design_vendor_rules_ocx.md`](./system_design_vendor_rules_ocx.md) (numbered contracts C-001…, scenarios S-001…, the full verification table, work packages)
**Reversibility:** one-way door **High** for the manifest (`vendor-manifest` v1) and the `.direct` sidecar grammar v1; two-way for everything else (the directory layout is internal, rules_ocx's surface is pre-1.0 and already breaking).

## Context

rules_ocx 0.5.1 fetches with `ocx pull` into a home, reads `ocx env`, renders POSIX launchers with absolute paths in Starlark and cannot run under remote execution: launchers re-enter `ocx`, paths are absolute, links are lost on upload. The owner's company adopts Bazel; its executors reach an internal mirror only. The source settles the shape: ocx gains one flat command per tier that writes a self-contained directory plus `DIR/manifest.json`, a native launcher that needs no `ocx`, and rules_ocx becomes a thin reader of that one document. What the source left open is listed under "Open-question decisions"; this ADR decides every entry.

Facts this design rests on (verified at `f41c6684` unless marked):

- The compat gate `ocx_sdkgen::compat::gate` walks exactly three documents (`crates/ocx_sdkgen/src/compat.rs:1169-1173`; `Documents` at `crates/ocx_sdkgen/src/lib.rs:85-92`). The other published schemas (`metadata`, `config`, `project`, `project-lock`, `patch`, `execution-record`) have a golden byte test only. Eight schemas are published (`crates/ocx_schema/src/main.rs:19`).
- `ensure_empty_or_absent` (`crates/ocx_util/src/fs/empty_or_absent.rs:59`) gives 64 for non-empty / not-a-directory and 74 for I/O.
- `ComposePaths`/`PathLane` (`crates/ocx_package_manager/src/composer.rs:769-893`) have two lanes and no output-directory lane; `ComposePaths::digest_only()` touches no filesystem; `LazyMode::Always` roots values in the home and writes a `ShimStore`.
- Assembly has one link call site (`crates/ocx_store/src/file_structure/assemble.rs:578`).
- `ocx_shim` is std-only off Windows (`crates/ocx_shim/Cargo.toml:14-21`); `run()` is `unimplemented!` off Windows; sidecar kinds are probed in `SIDECAR_PROBE_ORDER` (`crates/ocx_shim/src/core.rs:117`); each kind is a one-way door (`adr_windows_exe_shim.md`); blobs are committed and budgeted at 512 KiB (`crates/ocx_store/src/shim.rs:14`).
- `move_to_front`, `remove_segment` (`crates/ocx_util/src/path.rs`) and `append_unique` (`crates/ocx_util/src/list.rs`) are std-only; `ocx_util` itself is heavy and cannot be linked into the launcher. 15 files call them.
- `ocx exec` removes `Propagation::{Credential, HookLocal, State}` keys and writes `OCX_BINARY_PIN` and `__OCX_ENV_STATE` (`crates/ocx_package_manager/src/exec_env.rs:204-260`).
- Pulls on two machines agree on file bytes, file modes, link targets and file mtimes; directory modes follow the umask, directory and link mtimes follow the clock (`.agents/research/research_vendor-content-reproducibility-spike.md`).
- One cleartext nginx port carries h2c gRPC and HTTP/1.1; Buildbarn rejects unknown platform properties, so a cache silo is `--remote_instance_name=<prefix>/<key>`; with `--experimental_remote_repo_contents_cache` a fresh client skips the repository rule (0 registry requests, ~344 KB read vs 6 requests / 39.1 MB with it off) on Bazel 9.3.3 and 9.2.0 (`.agents/research/research_bazel-remote-repo-cache-rig-spike.md`). Tested only with links off disk.
- rules_ocx `ocx/extensions.bzl` invariant 3 forbids `module_ctx.os`/getenv in extension impls; invariant 1 forbids parsing ocx formats; `versions.bzl:28-35` refuses `X.Y.Z-pr<N>`; `dist.json` rows are GitHub archive URLs (`.claude/artifacts/discover_bazel_rbe_rules_ocx.md` § rules_ocx).

## Decision Drivers

1. **The owner's rules (binding):** a tool from a set brings the whole set; rules_ocx reaches ocx only through one documented, non-breaking interface and implements no ocx internals; ocx serves the new use from existing construction code, adjusted, with no second implementation; nothing in the directory or manifest is Bazel-specific; Bazel comes first where it costs the general interface nothing.
2. **Remote execution with network-less executors**, any backend that keeps symlink nodes in an input root.
3. **One-way doors are few and small:** the manifest and one sidecar grammar.
4. **Offline-first** (product principle 2): a Linux host vendors for macOS and Windows with no network beyond the package blobs.
5. **No hand-rolled wire-format parser** (`quality-core.md`, Block) and a launcher that stays std-only within 512 KiB.
6. **Reproducible output** across machines for everything a consumer digests.

## Industry Context & Research

**Research artifacts:** `.agents/research/research_launcher-lookup-prior-art.md`, `research_vendor-content-reproducibility-spike.md`, `research_bazel-remote-repo-cache-rig-spike.md`, and the list under the source's `## Research`. `research_bazel-hub-fetch-and-facts-size-spike.md` (spike 5, landed during this run, Bazel 9.2.0). Pending at writing: `research_launcher-runfiles-lookup-spike.md` (spike 6); claims that hinge on it carry "pending spike 6".

- **Native tool describes, Starlark stays thin:** rules_rust's crate_universe and rules_go let the native tool resolve and fetch; rules_python and rules_js evaluate their package manager in Starlark and chase its format. The owner's rule picks the first family.
- **Unpack in the repository rule:** rules_nodejs, rules_python, rules_java and toolchains_llvm do (source, read from source); Bazel uploads external-repository files as ordinary action inputs (source, run).
- **Toolchain declarations in a small hub, implementations in per-platform repositories:** bazel-lib's pattern. Spike 5 (Bazel 9.2.0): a hub with one `toolchain(exec_compatible_with = …)` per platform, or a `select()` alias, fetches only the selected platform's repository, for `build`, `fetch <target>` and `vendor <target>`; only `bazel vendor` with no target fetches all. A facts hit makes no extension call; a deleted lock makes one.
- **One identical binary plus a sidecar named after it:** Scoop's `shim.exe` + `<name>.shim`; rustup dispatches one binary by name. Bazel's own Windows launcher appends data to the PE and so changes bytes per target, which an ad-hoc macOS signature forbids (`research_launcher-lookup-prior-art.md` §3-4).
- **NUL-delimited records carry arbitrary strings with no escaping:** `/proc/<pid>/environ`, `find -print0`/`xargs -0`, systemd's environment files. NUL cannot occur in a path, an argument or an environment value on Linux, macOS or Windows.
- **Placeholder substitution:** autoconf and CMake `configure_file` use `@VAR@` tokens replaced by plain string search.
- **Runfiles discovery:** rules_rust's `Runfiles::create()` order is explicit manifest/dir → `RUNFILES_MANIFEST_FILE` → `RUNFILES_DIR` → `TEST_SRCDIR` → `argv[0]` sibling `.runfiles`; the unresolved own path matters because `/proc/self/exe` resolves symlinks.

**Key insight:** every hard property (relocatable, network-less, shell-less, signature-stable, cross-host) is met if the launcher bytes never change and all per-tool state is a sidecar whose grammar needs no escaping and whose paths are relative to a token.

## Considered Options and trade-off matrices

Weights: 5 = binding requirement, 3 = strong driver, 1 = nice to have. Score 1-5; total = Σ weight × score.

### Choice 1 — how the persisted manifest is held against breaks

| Criterion (weight) | A. Fourth gated document `vendor-manifest` | B. Golden byte test only (like `project-lock`) | C. Pseudo report root inside `reports.json` |
|---|---|---|---|
| Fails on a break without a version bump (5) | 5 | 1 (golden is re-blessed by a regenerate) | 5 |
| Own versioned URL `schemas/<name>/v<N>.json` like the other specs (5) | 5 | 5 | 1 |
| Change size in `ocx_sdkgen` (1) | 3 (new `Kind`, `Documents` field, baseline) | 5 | 5 |
| SDK generators see a coherent document set (3) | 5 | 3 | 2 (a file posing as stdout) |
| **Total** | **73** | 49 | 46 |

**Chosen: A.** `ocx_sdkgen` gains `Kind::VendorManifest`, `Documents.vendor_manifest`, baseline `crates/ocx_schema/contract/baseline/vendor-manifest.json`, ledger `document = "vendor-manifest"`, and the version read from `/properties/schema_version/const` the way the error document's is. Lint rules: the reports rule set minus the report-root rules (L09, L13, L14).

### Choice 2 — launcher sidecar data format for the new kind

| Criterion (weight) | A. NUL-delimited records, no escaping | B. JSON parsed by `serde_json` in the launcher | C. Data appended to the launcher binary |
|---|---|---|---|
| No hand-rolled wire parser (5) | 5 (parser is `split(0)`; no escape, number or nesting exists) | 5 | 3 |
| Launcher bytes identical for every tool, signature intact (5) | 5 | 5 | 1 |
| Launcher stays std-only, within 512 KiB (3) | 5 | 2 | 5 |
| Readable by a human (1) | 2 | 5 | 1 |
| **Total** | **67** | 61 | 38 |

**Chosen: A**, extension `.direct`. One codec, encoder and decoder, lives in the new leaf crate both writer and launcher link (Choice 4), so there is one implementation and nothing to drift.

### Choice 3 — where a launcher for a platform that is not the host's comes from

| Criterion (weight) | A. All six blobs embedded in every `ocx` | B. Launcher published as an ocx package, fetched at vendor time | C. Host blob only, refuse cross-platform |
|---|---|---|---|
| Linux host vendors for macOS and Windows (5) | 5 | 5 | 1 |
| Offline-first, no extra fetch, no release-order coupling (3) | 5 | 2 (dev builds must publish it too; pin compiled into ocx) | 5 |
| Binary size (1) | 3 (≤ 6 × 512 KiB budget, measured below it) | 5 | 5 |
| Fits existing provenance + SHA-canary gate (3) | 5 | 3 | 5 |
| **Total** | **58** | 46 | 39 |

**Chosen: A** (DV-3). Linux blobs are static musl (`x86_64-`/`aarch64-unknown-linux-musl`), so one blob per architecture runs on glibc, musl and a shell-less static image.

### Choice 4 — one env-apply implementation for `ocx exec` and the launcher

| Criterion (weight) | A. New std-only leaf crate `ocx_env_apply`, both link it | B. A copy in the launcher, held by a parity test |
|---|---|---|
| No second implementation (owner rule) (5) | 5 | 1 |
| Launcher size/std-only (3) | 5 | 5 |
| Change cost (1) | 2 (15 caller files, mechanical) | 5 |
| **Total** | **42** | 25 |

**Chosen: A.** The parity test (V-22) still runs, as the source requires in both cases.

### Choice 5 — how package files reach the directory

| Criterion (weight) | A. Copy, then normalise modes and mtimes | B. Hardlink from the home's layer store | C. Reflink where the filesystem allows |
|---|---|---|---|
| Directory independent of a home, any device (5) | 5 | 1 (`CrossesDevices`, edits leak into the home) | 3 |
| Byte- and digest-stable across machines (5) | 5 | 3 | 4 |
| Disk and time cost (1) | 2 | 5 | 4 |
| **Total** | **52** | 25 | 39 |

**Chosen: A.** `LinkMode::{Hardlink, Copy}` on the one call site `assemble.rs:578`; existing callers keep `Hardlink`.

### Choice 6 — how rules_ocx's CI gets the unreleased `ocx` of the ocx pull request

| Criterion (weight) | A. New `OCX_INSTALL_BINARY` path variable read by `ocx_download` | B. A hand-made dist manifest served by the rig | C. Relax `version` to accept `-pr<N>` and add an OCI row type |
|---|---|---|---|
| Works for the dev build at `ghcr.io/ocx-sh/dev/cli` (OCI only, no archive) (5) | 5 | 2 (needs archives made by hand) | 4 |
| Touches no released contract (`dist.json` schema 1) (3) | 5 | 5 | 1 |
| Also serves every CI leg, macOS and Windows included (3) | 5 | 2 | 5 |
| **Total** | **55** | 31 | 38 |

**Chosen: A.** Fits beside the existing `OCX_INSTALL_DIST_URL` and `OCX_INSTALL_MIRROR_URL`. CI pulls the dev build with a released ocx (`ocx package pull ghcr.io/ocx-sh/dev/cli:<X.Y.Z-prN>@<digest>`) and exports the path.

### Choice 7 — launcher root lookup

| Criterion (weight) | A. `RUNFILES_DIR` → `<own>.runfiles` → lexical parent, each accepted only if it holds a byte-identical copy of the sidecar | B. Lexical parent first, then runfiles (source order), then runfiles manifest | C. Starlark writes a root hint |
|---|---|---|---|
| Links visible (they exist only in the runfiles tree) (5) | 5 | 3 (a parent of a bazel-out executable can match first) | 5 |
| No Bazel term in directory or manifest (5) | 5 | 5 | 1 (and Starlark writes launcher data) |
| Works outside Bazel and after a move (5) | 5 | 5 | 2 |
| Windows manifest-only runfiles (1) | 1 | 4 | 4 |
| **Total** | **76** | 69 | 44 |

**Chosen: A** (DV-6). Spike 6 (`.agents/research/research_launcher-runfiles-lookup-spike.md`) confirmed it on Linux: `<own>.runfiles/<repo>` finds the root in every genrule, `bazel run` and `bazel test` leg, the lexical parent finds it in a moved vendored directory and for a bare-name start through `PATH`, and a canonicalised own path (`/proc/self/exe`) fails in every Bazel leg and picks Bazel's repo contents cache as the root in a copied directory. Windows stays the CI check V-45.

## Decision Outcome

**Chosen:** ocx ships `ocx vendor` and `ocx package vendor`, the `vendor-manifest` v1 schema as a fourth gated document, a `.direct` launcher kind built for six targets from one std-only crate, and the leaf crate `ocx_env_apply`. rules_ocx becomes one repository rule per set and platform that runs `ocx vendor`, a hub generated from facts produced by `ocx vendor --manifest-only`, and a Buildbarn + NativeLink rig in its CI.

### What a run writes (layout internal; the manifest names every path)

```
DIR/manifest.json
DIR/packages/<registry>/<algorithm>/<2 hex>/<30 hex>/content/…               one per package digest, once
DIR/packages/<registry>/<algorithm>/<2 hex>/<30 hex>/entrypoints/<name>[.exe] + <name>.direct
DIR/selections/default/shells/default/bin/<name>[.exe] + <name>.direct      one per tool of the selection
```

Nothing under `refs/`, no `metadata.json`, `resolve.json`, `install.json`, `digest` or OCI `manifest.json` per package (DV-1). No `active` link and no `links/<group>/<entry>` tree in a selection (DV-2).

### API contract (CLI)

```
ocx [--offline] [--blob-dir D]… [--blob-cache-dir D] [--project P | --global] vendor
    -o|--output DIR  [-p|--platform PLATFORM]  [-g|--group GROUP]…  [--manifest-only]  [--omit-links]  [NAME]…
ocx [global flags] package vendor
    -o|--output DIR  [-p|--platform PLATFORM]  [--manifest-only]  [--omit-links]  PACKAGE…
stdout: one `VendorReport` (plain table, or JSON under --format json); never the manifest
```

### Data model (manifest v1, abridged; full schema and example in the system design § 4)

```
VendorManifest { schema_version: 1, content: present|absent, platform, available_platforms[], dir_token: "@VENDOR_DIR@",
                 packages[ {identifier, digest, package_dir, content_dir, dependencies[], layers[{digest,size,media_type}],
                            names_declared, entrypoints[{name, launcher_path, environment}],
                            tree?{files[{path,executable}], links[{path,target}], link_free_dirs[], empty_dirs[]}} ],
                 environments[ {id, scope{type: selection|entrypoint, …}, items[{key, value, kind, separator?}]} ],
                 selections[ {id, groups[], bindings[], roots[], environment, launcher_dir} ],
                 tools[ {name, selection, kind: binary|entrypoint, package, environment, target_path, launcher_path?} ] }
```

### Quantified impact

| Metric | Before (0.5.1 shape, spike) | After (target) | Notes |
|---|---|---|---|
| Build on network-less executors | fails (launchers re-enter `ocx`) | passes | V-01 |
| Fresh client, warm remote cache, repo contents cache on | rule runs, 6 req / 39.1 MB | 0 req, ~344 KB read | measured on the spike's rule shape; V-03 re-measures with `ocx vendor` |
| `ocx` in an action's inputs | always | never | V-06 |
| Cold fetch of one tool (node) | 3.0 s rule run | ≤ 1.2 × pull time | copy plus normalise; measured by the baseline task |
| Per-`ocx` binary size | Windows: 1 blob | + ≤ 6 × 512 KiB | budget asserted per blob |
| Facts in `MODULE.bazel.lock` | none | ≤ 15 KB for this repository's 20 tools × 4 platforms | spike 5 measured 13263 B JSON / 14714 B escaped with digests and entry-point names included; the v1 facts carry less |

### Consequences

**Positive:** one interface for Bazel and later consumers (shell files, installer renderers) with no ocx internals in Starlark; network-less, shell-less executors work; the whole set travels with every tool; a vendored directory is also the movable install the owner missed in import.

**Negative:** a second representation of every package on disk (copy, not hardlink); every action using a tool of a set keys on the whole set (known, today's behaviour); the vendored launcher writes no `__OCX_ENV_STATE`, so an `ocx exec --isolate` nested under a vendored tool cannot revert the vendored values (it warns, `CannotIsolate`); rules_ocx's next release breaks every `bins` user and drops Bazel 8.

**Risks and mitigations:**

| Risk | Mitigation |
|---|---|
| A Bazel source-directory label behaves differently from file labels for a backend | V-34 compares input lists in the execution log on both backends |
| Repo contents cache skips repositories that keep symlinks (issue 31456) | links off disk by default (`--omit-links`, the tested form); `links_on_disk` attribute is opt-in and documented as uncached |
| Facts key collision (`hash()` is 32-bit) | key = name + 32-bit hash + byte length of `ocx.lock` + groups + names + platform; the set repository compares its real manifest's tool names with the hub's facts and `fail()`s naming `bazel mod deps --lockfile_mode=update` (C-RO-12) |
| Cross-host rendering differs (Windows separators, `:native`) | vendor lane renders `/` after the token always; `:native` renders the *target* separator; V-46 compares a Linux-host and a Windows-host manifest byte for byte |
| Unsigned darwin Mach-O content vendored on Linux stays unsigned | stated limit; all 12 sampled Mach-O files already carry signatures; ocx signs only on a macOS host |
| Windows runfiles without a tree (`--enable_runfiles` off) | launcher exits 78 naming `--enable_runfiles`; rules_ocx docs require it on Windows (DV-6) |

## Open-question decisions

Every entry of the source's `## Open questions` (lines 627-720), in source order.

| ID | Question | Decision | Reason |
|---|---|---|---|
| OQ-01 | Mach-O bytes of macOS ad-hoc signing identical across machines? | Not needed for the launcher (committed bytes, never re-signed); for content, a macOS CI check pulls node, shfmt and python-build-standalone darwin/arm64 with and without `OCX_NO_CODESIGN` and compares every sha256 (V-27b) | ocx re-signs only Mach-O whose signature fails; all sampled ones verify |
| OQ-02 | Which backends keep symlink nodes in an input root? | Buildbarn hardlinking worker and NativeLink 1.9 in the rig (V-12); the conformance test stays rig-only (OQ-31) | run on 2026-11-25; hosted services unchecked and named as such in the docs |
| OQ-03 | How does the launcher find its trees inside an action? | Choice 7 A; full algorithm C-012 | links live only in the runfiles tree; confirmed on Linux by spike 6, Windows is V-45 |
| OQ-04 | Is the unpacked repository identical across machines? | Yes after normalisation C-008 (dirs 0755, dir and link mtimes 315532800); V-09 compares execution-log input digests from two machines | umask and clock are the only differences measured |
| OQ-05 | Does `package export` fetch dependency blobs per dependent? | Falls away: the rule does not run export | ratified proposal |
| OQ-06 | What home does the repository rule give `ocx`? | rules_ocx's existing `make_ocx_env` (`$OCX_HOME`, else `$HOME/.ocx`, else `isolated_home`); the home is only a download cache, because copy mode makes the directory independent of it | reuses the client's layer cache; the repository's content does not depend on the home |
| OQ-07 | Fresh client with warm remote cache, repo contents cache on/off? | Measured (spike S-B): on 0 req / ~344 KB, off 6 req / 39.1 MB; docs recommend the flag from 9.3.0; V-03 re-runs it with `ocx vendor` | the spike measured the cache, not ocx |
| OQ-08 | How does a tool find an entry point of another package by bare name? | The composed `PATH` already holds each package's `entrypoints/` directory (`composer.rs:400`), and the vendor writes direct launchers there | no extra launcher directory, no new composition (V-30) |
| OQ-09 | Package content holding `BUILD`, `BUILD.bazel`, `MODULE.bazel`, `REPO.bazel`, `WORKSPACE*` | ocx writes it unchanged and lists it in `tree.files`; rules_ocx renames it in the repository and declares it back at its place as an output, as it does for links (DV-4) | a renaming flag in ocx is a Bazel term in the interface |
| OQ-10 | Empty directories kept? | ocx writes them and lists them in `tree.empty_dirs`; rules_ocx declares none in the first release; a rig fixture records what a directory label does with one | fidelity for non-Bazel consumers; 2 of 69 packages have one |
| OQ-11 | What goes into facts, how large? | Per set: `available_platforms`; per set and platform: `schema_version` and `[name, kind]` per tool; key C-RO-10 | the hub needs names only; spike 5 measured ≈ 13 KB for 20 tools × 4 platforms with more fields than these |
| OQ-12 | Composed environment a fact or joined in Starlark? | Neither: ocx composes it into the manifest | ratified |
| OQ-13 | Command family name and verbs | `ocx vendor`, `ocx package vendor`, flag `--manifest-only` | ratified |
| OQ-14 | Manifest with no files written | `--manifest-only` writes `DIR/manifest.json` alone, `content: "absent"` | ruled 2026-11-26 |
| OQ-15 | Default for `-o` | none, required | ruled |
| OQ-16 | Target exists and is not empty | refused through `ensure_empty_or_absent`: exit 64 (`vendor_output_not_empty`), 74 on I/O; nothing written | the `package test --output` rule; no default means a rerun is the caller's choice |
| OQ-17 | Manifest to stdout? | No | ruled |
| OQ-18 | Manifest location | `DIR/manifest.json` | ruled |
| OQ-19 | How is the file held against breaks? | Choice 1 A | gate must fail on a break |
| OQ-20 | What does the command print? | `VendorReport` v1: `{schema_version, output_dir, manifest_path, content, platform, packages[{identifier, digest}], tool_count, written_size}` | as `ExportReport`; one place to read the manifest |
| OQ-21 | Second executable? | No | ruled |
| OQ-22 | Directory labels without walking, in non-Bazel terms | `tree.links` with targets and `tree.link_free_dirs` (top-most directories with no link below), plus `tree.files` (DV-5) | plain facts about a tree |
| OQ-23 | Identity fields per package | `identifier` (pinned, tag when one was given), `digest`, `layers`, `dependencies`; descriptive fields come from `ocx package description pull` | installers need them; the first release needs none (V-37 table) |
| OQ-24 | Shell files written or printed? | Deferred | allowed by the owner |
| OQ-25 | More than one selection per run? | No; `selections`, `environments` and `tools` are lists | ruled |
| OQ-26 | Env application without drift | Choice 4 A | owner's reuse rule |
| OQ-27 | Variables `ocx exec` removes or writes | The `.direct` sidecar carries `unset` records written from `ocx_env::keys_with(Credential, HookLocal, State)` plus `OCX_BINARY_PIN`; the launcher writes neither `__OCX_ENV_STATE` nor `OCX_BINARY_PIN`; mode is Layer (on top of the caller's environment), as `ocx exec`'s default | one source for the list; no `ocx` to pin |
| OQ-28 | Copy or hardlink | Choice 5 A | independence from the home |
| OQ-29 | Kept package files | `content/` and the package's `entrypoints/` launchers with sidecars; nothing else (DV-1) | rules_ocx reads none; everything else holds absolute paths or home state |
| OQ-30 | Package declaring neither binaries nor entry points | `NotEnumerablePolicy::Skip`: no tools, still present in `packages` and in the environment; `names_declared: false` in both modes | same as the toolchain renderer; names come from metadata only |
| OQ-31 | Patches and client configuration | Patches never reach a vendored environment, whatever `[patches]`, `OCX_PATCHES` or `OCX_PATCH_SNAPSHOT` say; configuration that only routes bytes (mirrors, registries, auth, trust policy) applies | a vendor is a render; renders do not model companions (subsystem rule); routing is digest-verified and cannot change content |
| OQ-32 | Signature on every path on Apple Silicon | Prove in ocx CI on `macos-latest` (arm64): hardlink under another name, copy, rename, tar round trip; control: one byte changed is killed (V-27); `bazel vendor` round trip in rules_ocx's macOS leg | no macOS host here |
| OQ-33 | Launcher for a non-host platform | Choice 3 A | offline-first |
| OQ-34 | Installed-home launchers move now? | Later, under [ocx-sh/ocx#679](https://github.com/ocx-sh/ocx/issues/679); on Unix the program accepts only `.direct` until then (others exit 78) | ruled |
| OQ-35 | darwin content needing signing, vendored on Linux | Stated limit of the first release | ocx signs only on a macOS host |
| OQ-36 | Marker for the directory in a value | The literal token `@VENDOR_DIR@`, replaced by plain search; a resolved value that already contains it, or contains NUL, is refused (exit 65, `vendor_value_unrepresentable`) | no escape syntax exists, so none can be got wrong |
| OQ-37 | Does `inspect` need an addition? | No | ratified |
| OQ-38 | Declared symlinks on a Windows executor | Not in the first release: a set repository for a `windows/*` platform whose manifest lists links `fail()`s naming the package | no Windows package with links is known |
| OQ-39 | Platform string | The full canonical string (`linux/amd64+libc.glibc`); rules_ocx takes `available_platforms` from the manifest and passes each verbatim. `available_platforms` is the intersection: the platforms on which every binding of the selection has a leaf; an explicit `platforms` entry outside it fails naming the binding (ocx's existing exit 78) | `-p linux/amd64` fails for libc-only publishers; spike 5: a five-group set resolves on 4 of 10 lock keys |
| OQ-40 | Repositories of unused platforms fetched? | Hub holds `toolchain()` declarations only (`exec_compatible_with`), implementations in per-platform repositories; the hub BUILD names a set repository only in labels analysis follows | spike 5: only the selected platform's repository is fetched |
| OQ-41 | `bazel run` export target | Dropped | `bazel vendor` and `blob_dirs` cover offline |
| OQ-42 | Credential sources inside a repository rule | The client's ocx and Docker configuration via the inherited environment; no secret in `MODULE.bazel.lock`, a repository, an action key or an execution log (V-10) | secrets stay on the client |
| OQ-43 | Physical blob URL in `package inspect` | Not added now; the manifest's `layers` carries digest, size and media type for the later native download (DV-9) | native download out of the first release |
| OQ-44 | Supported Bazel 9 releases | All 9.x; `bazel_compatibility = [">=9.0.0"]`; the cache flag recommended from 9.3.0 | 9.2.0 did not hang in the spike, the trigger was not provoked |
| OQ-45 | Content-tree command for the first release? | Falls away: `ocx vendor` is it | ratified |
| OQ-46 | Conformance test shipped or rig-only? | Rig-only (V-12) | a broken backend fails loudly on the first package with links |

`[NEEDS CLARIFICATION: Does NativeLink admit an arbitrary instance-name suffix as a per-test cache silo, as Buildbarn does?]` Recommended default if not: a per-test `--action_env=OCX_RIG_SILO=<key>` salt plus a repo-contents-cache-free leg; decided in the rig, does not change ocx.

`[NEEDS CLARIFICATION: Does a Bazel source-directory label keep an empty directory and a nested BUILD file below it on both backends?]` Recommended default: treat neither as kept (OQ-09, OQ-10 already route around both); a rig fixture answers it.

## Divergences from the source

| ID | Source says | This ADR | Strong reason |
|---|---|---|---|
| DV-1 | Whether `metadata.json`, `manifest.json`, `resolve.json`, `install.json`, `digest` go along is the ADR's call | none of them | rules_ocx reads none; they describe a home, and `refs/` is absolute |
| DV-2 | Selection directory "as in a rendered toolchain home (`links/<group>/<entry>`, `shells/`)" | `shells/default/bin/` only; no `active`, no `links/` | both are aliases to package roots the manifest already names; additive later |
| DV-3 | Non-host launcher "published per platform as an ocx package" (recommended) | embedded in every `ocx` (Choice 3) | offline-first, no release-order coupling, dev builds carry them for free |
| DV-4 | ocx "stores such a file under another name on request" | rules_ocx renames after the run | the flag would be a Bazel term in ocx's interface |
| DV-5 | List links and link-free directories; "the alternative, a list of every file, makes the document large" | both, plus `tree.files` | reserved names become detectable with no walk and no Bazel term; installers need file lists; the size lands in the repository's file, never in facts |
| DV-6 | Lookup: started path, then `.runfiles`, then the runfiles manifest | `RUNFILES_DIR`, `<own>.runfiles`, lexical parent; no manifest-file mode | links exist only in a runfiles tree; a manifest-only tree has no directory to substitute for the token |
| DV-7 | "rules_ocx runs the command … and reads only the manifest" with the extension untouched | extension runs `ocx vendor --manifest-only`; invariant 3 amended | facts need it (brief: deliberate, recorded amendment) |
| DV-8 | Patches: "the same composer brings patches in when the client configures them" (recommended) | never (OQ-31) | a vendored directory must not depend on site overlays; rules_ocx's `patch_snapshot` attribute goes |
| DV-9 | V-16: "a digest and its blob data as plain attributes" | digest as a plain attribute; blob data specified in the manifest (`layers`) only | the native download that consumes it is out of the first release (2026-11-23) |
| DV-10 | V-17: "This repository builds with the new rules_ocx" | shown with `--override_module=rules_ocx=<checkout>` in the run; the committed `MODULE.bazel` move waits for the rules_ocx release | the release order the source states makes a committed pin impossible inside the run |
| DV-11 | Owner's grammar lists `-o`, `-g`, `NAME`, `--manifest-only` | adds `-p` and `--omit-links` | the ratified proposal has "one platform" and "on request it leaves the links off the disk"; these are their spellings |

## Component contracts (summary; numbered in full in the system design)

1. **`ocx vendor`** (C-001…C-011): exits 64 without `-o` and writes nothing; refuses a non-empty `DIR` (64) before any network; writes `DIR/manifest.json` last via temp-and-rename so a crashed run leaves no manifest; `--manifest-only` writes exactly one file and fetches no layer blob; manifest-only and full manifests are equal except `content` and `tree`; every environment equals `ocx env --pinned` for the same selection with the home's package paths replaced by `@VENDOR_DIR@/…` and patches absent; every vendored file equals the installed `content/` in bytes, executable bit and link target; directory modes 0755, directory and link mtimes 315532800; manifest bytes do not depend on host OS, user, umask, clock or `OCX_HOME`.
2. **`.direct` launcher** (C-012…C-020): finds root per Choice 7, accepting a candidate only if `<candidate>/<self>.direct` is byte-identical to the sidecar it read; makes the root absolute by lexical join with the working directory (no symlink resolution); removes every `unset` key, applies `path`/`list`/`constant` records in file order through `ocx_env_apply`; refuses any program or substituted `path` value that leaves the root (77); Unix `execve`, Windows `CreateProcessW` with the existing job object and exit-code passthrough; writes nothing anywhere; 78 for a missing or malformed sidecar naming every path tried.
3. **rules_ocx** (C-RO-01…C-RO-14): runs no ocx command other than `ocx vendor` and `ocx package vendor`, reads no file of ocx other than `manifest.json`, names no path inside an ocx home and no package metadata field (guard script red on one planted violation); one repository per set and platform; one hub with `toolchain()` per platform and one target per tool name, built from facts with no ocx call when facts exist.

## NFRs

- **Scalability:** manifest grows ≈ 60 B per file (node: 4800 files ≈ 300 KB, in the repository file only); facts ≈ 30 B per tool name per platform; a set of N packages is one repository per platform, so the remote cache stores each file once.
- **Availability:** works with no network given `--blob-dir` or `bazel vendor`; the launcher depends on no service and no `ocx`.
- **Latency:** `--manifest-only` ≤ `inspect --resolve --closure` (1.9 s for four bindings); a vendor run ≤ 1.2 × the pull time of the same set; launcher overhead ≤ 5 ms to `execve` on Linux (measured by the baseline task, not asserted in CI).
- **Security:** credential, hook-local and state variables never reach a child; a sidecar holds no secret by construction (keys only for unset); containment on every substituted path (77); NUL-delimited format refuses nothing that could be smuggled because NUL cannot occur in a value; launcher bytes immutable (macOS signature intact); no secret in facts, repository, action key or execution log (V-10).
- **Cost:** + ≤ 3 MiB per `ocx` binary; the first remote upload is the unpacked size (144 MB vs a 59 MB archive for node); rig CI job ≈ 10 min on one Linux runner.
- **Operability:** every failure maps to an existing exit code and a new `error.detail` slug; rules_ocx maps every reachable sysexit to a `fail()` hint (its invariant 4); the rig's nginx JSON log is the witness for registry bytes.

## C4 sketch

**Context.** A developer or CI runs Bazel on a client. The client reaches an OCI registry or mirror (auth via the client's Docker/ocx config) and a REAPI backend. Executors reach nothing. ocx runs on the client only.

**Containers.** On the client: Bazel, the rules_ocx module extension (runs `ocx vendor --manifest-only`, writes facts to `MODULE.bazel.lock`), the hub repository (toolchain declarations + tool targets), one set repository per set and platform (runs `ocx vendor`, holds the vendored directory), the `ocx` binary from `@ocx_tool`. On executors: the uploaded set files, declared symlinks and the native launcher.

**Components (ocx).** `ocx_cli::command::{vendor, package_vendor}` → `ocx_package_manager::tasks::vendor` → existing `select_tool_set`/`resolve_selected_tools`, `compose_roots` with `PathLane::Vendor`, `toolchain_surface` + `exposed_names`, `pull` with `dest_override` + `LinkMode::Copy`, `render_toolchain`'s direct target kind and `launcher::generate`'s direct kind → `ocx_package::vendor::VendorManifest` (types) → `ocx_schema` (schema) → `ocx_sdkgen` (gate). `ocx_shim` + `ocx_env_apply` form the launcher; `ocx_store::shim` embeds six blobs.

## Validation

- [ ] Every row of the system design § 9 shown red once and green once
- [ ] Security review of the launcher (containment, env removal, Windows spawn) by the security-auditor skill
- [ ] `task satellite:verify` green with the `ocx_env_apply` move and the `ocx_package::vendor` addition

## Links

- [`system_design_vendor_rules_ocx.md`](./system_design_vendor_rules_ocx.md)
- [`adr_windows_exe_shim.md`](./adr_windows_exe_shim.md), [`adr_package_entry_points.md`](./adr_package_entry_points.md), [`adr_exec_env_model.md`](./adr_exec_env_model.md), [`adr_ocx_interface_contract.md`](./adr_ocx_interface_contract.md), [`adr_crate_split_workspace.md`](./adr_crate_split_workspace.md), [`adr_package_export_import.md`](./adr_package_export_import.md), [`adr_cli_plugin_pattern.md`](./adr_cli_plugin_pattern.md)
- [`discover_bazel_rbe_rules_ocx.md`](./discover_bazel_rbe_rules_ocx.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-11-26 | architect worker | Initial draft (Proposed) |
