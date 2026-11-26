# ADR: `ocx vendor`, its manifest and a native launcher, consumed by rules_ocx under remote execution

## Metadata

**Status:** Proposed
**Date:** 2026-11-26
**Deciders:** architect worker of a `/hex-architect` run (tier high), inside an unattended goal loop; orchestrator rulings R-1…R-8 (goal-loop authority); the owner rules on the result
**Beads Issue:** N/A
**Related PRD:** `.agents/discussions/bazel-rbe-rules-ocx.md` (ratified 2026-11-26, the "source" below)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, Tokio, no new language or runtime)
- [x] Deviations justified below (DV-1 … DV-13)
**Domain Tags:** api | integration | infrastructure | security | devops
**Supersedes:** the Bazel decisions of `.agents/discussions/bazel-hermetic-package-bundles.md` that the source reverses (extraction in a build action, one repository per package, the repository rule running `ocx package export`)
**Superseded By:** —
**Companion:** [`system_design_vendor_rules_ocx.md`](./system_design_vendor_rules_ocx.md) (contracts C-001…C-024 and C-RO-01…C-RO-17, scenarios S-001…S-015, the verification table V-01…V-73, work packages)
**Reversibility:** one-way door **High** for the manifest (`vendor-manifest` v1). The `.direct` sidecar is versioned (`schema_version`) and always written together with the launcher bytes of the same ocx release, so inside a vendored directory it is two-way; it becomes one-way only once [ocx-sh/ocx#679](https://github.com/ocx-sh/ocx/issues/679) puts it into installed homes. Everything else is two-way (the directory layout is internal; rules_ocx's surface is pre-1.0 and already breaking).

## Context

rules_ocx 0.5.1 fetches with `ocx pull` into a home, reads `ocx env`, renders POSIX launchers with absolute paths in Starlark and cannot run under remote execution: launchers re-enter `ocx`, paths are absolute, links are lost on upload. The owner's company adopts Bazel; its executors reach an internal mirror only. The source settles the shape: ocx gains one flat command per tier that writes a self-contained directory plus `DIR/manifest.json`, a native launcher that needs no `ocx`, and rules_ocx becomes a thin reader of that one document. This ADR decides every entry the source left open (OQ table).

Facts this design rests on (verified at `f41c6684` unless marked):

- The compat gate `ocx_sdkgen::compat::gate` walks exactly three documents (`crates/ocx_sdkgen/src/compat.rs:1169-1173`; `Documents` at `crates/ocx_sdkgen/src/lib.rs:85-92`). The other published schemas have a golden byte test only. Eight schemas are published (`crates/ocx_schema/src/main.rs:19`).
- `ensure_empty_or_absent` (`crates/ocx_util/src/fs/empty_or_absent.rs:59`) gives 64 for non-empty / not-a-directory and 74 for I/O.
- `ComposePaths`/`PathLane` (`crates/ocx_package_manager/src/composer.rs:769-893`) have two lanes and no output-directory lane; `ComposePaths::digest_only()` touches no filesystem; `LazyMode::Always` roots values in the home and writes a `ShimStore`.
- Assembly has one link call site (`crates/ocx_store/src/file_structure/assemble.rs:578`).
- `ocx_shim` is std-only off Windows (`crates/ocx_shim/Cargo.toml:14-21`); `run()` is `unimplemented!` off Windows; kinds are probed in `SIDECAR_PROBE_ORDER` (`crates/ocx_shim/src/core.rs:117`); blobs are committed and budgeted at 512 KiB (`crates/ocx_store/src/shim.rs:14`); the two committed Windows blobs are 239 KiB and 333 KiB.
- `move_to_front`, `remove_segment` (`crates/ocx_util/src/path.rs`) and `append_unique` (`crates/ocx_util/src/list.rs`) are std-only. 15 caller files in seven crates use them: `ocx_cli`, `ocx_config`, `ocx_package`, `ocx_package_manager`, `ocx_project` (`src/env.rs`), `ocx_setup`, `ocx_shell` (the list is in the system design's A1). `ocx_util` is heavy and cannot be linked into the launcher.
- `ocx exec` removes `Propagation::{Credential, HookLocal, State}` keys, passes `SecretInput` variables through, and writes `OCX_BINARY_PIN` and `__OCX_ENV_STATE` (`crates/ocx_package_manager/src/exec_env.rs:204-260`).
- Pulls on two machines agree on file bytes, file modes, link targets and file mtimes; directory modes follow the umask, directory and link mtimes follow the clock (`research_vendor-content-reproducibility-spike.md`).
- One cleartext nginx port carries h2c gRPC and HTTP/1.1; Buildbarn rejects unknown platform properties, so a cache silo is `--remote_instance_name=<prefix>/<key>`; with `--experimental_remote_repo_contents_cache` a fresh client skips the repository rule (0 registry requests, ~344 KB read vs 6 requests / 39.1 MB off) on 9.3.3 and 9.2.0 (`research_bazel-remote-repo-cache-rig-spike.md`). Tested only with links off disk.
- A JSON sidecar parsed by `serde_json` builds to 365 KiB against 316 KiB std-only, `x86_64-unknown-linux-gnu`, shim profile (`research_launcher-sidecar-json-size.md`); musl, macOS and Windows sizes unmeasured.
- rules_ocx invariant 3 forbids `module_ctx.os`/getenv in extension impls; invariant 1 forbids parsing ocx formats; `versions.bzl:28-35` refuses `X.Y.Z-pr<N>`; `dist.json` rows are GitHub archive URLs; Starlark has no sha256 function (`discover_bazel_rbe_rules_ocx.md` § rules_ocx).

## Decision Drivers

1. **The owner's rules (binding):** a tool from a set brings the whole set; rules_ocx reaches ocx only through one documented, non-breaking interface and implements no ocx internals; ocx serves the new use from existing construction code, adjusted, with no second implementation; nothing in the directory or manifest is Bazel-specific; Bazel comes first where it costs the general interface nothing.
2. **Remote execution with network-less executors**, any backend that keeps symlink nodes in an input root.
3. **One-way doors are few and small.**
4. **Offline-first** (product principle 2): a Linux host vendors for macOS and Windows with no network beyond the package blobs.
5. **No hand-rolled wire-format parser** (`quality-core.md`, Block) and a launcher within 512 KiB per target.
6. **Reproducible output** across machines for everything a consumer digests.
7. **Security parity with `ocx exec`** and no new injection surface on Windows.

## Industry Context & Research

**Research artifacts:** `.agents/research/research_launcher-lookup-prior-art.md`, `research_vendor-content-reproducibility-spike.md`, `research_bazel-remote-repo-cache-rig-spike.md`, `research_bazel-hub-fetch-and-facts-size-spike.md` (spike 5), `research_launcher-runfiles-lookup-spike.md` (spike 6), `research_launcher-sidecar-json-size.md`, and the list under the source's `## Research`.

- **Native tool describes, Starlark stays thin:** rules_rust's crate_universe and rules_go let the native tool resolve and fetch; rules_python and rules_js evaluate their package manager in Starlark and chase its format. The owner's rule picks the first family.
- **Unpack in the repository rule:** rules_nodejs, rules_python, rules_java and toolchains_llvm do; Bazel uploads external-repository files as ordinary action inputs (source).
- **Toolchain declarations in a small hub, implementations in per-platform repositories** (bazel-lib). Spike 5 (Bazel 9.2.0): one `toolchain(exec_compatible_with = …)` per platform, or a `select()` alias, fetches only the selected platform's repository for `build`, `fetch <target>` and `vendor <target>`; only `bazel vendor` with no target fetches all. A facts hit makes no extension call.
- **One identical binary plus a sidecar named after it:** Scoop's `shim.exe` + `<name>.shim`; rustup dispatches one binary by name. Bazel's own Windows launcher appends data to the PE and so changes bytes per target, which an ad-hoc macOS signature forbids (`research_launcher-lookup-prior-art.md` §3-4).
- **Placeholder substitution:** autoconf and CMake `configure_file` use `@VAR@` tokens replaced by plain string search.
- **Runfiles discovery:** rules_rust's `Runfiles::create()` uses `RUNFILES_DIR` and the `argv[0]` sibling `.runfiles`. Spike 6: the unresolved started path (when it ends with the sidecar's `self`) and then `<started path>.runfiles/<entry>/` find the root in every Linux leg (genrule sandboxed and local, `--nobuild_runfile_links`, `bazel run`, `bazel test`, a moved directory, a bare-name start); a canonicalised own path (`/proc/self/exe`) fails every Bazel leg and picks Bazel's repo contents cache in a copied directory. The links must be declared by a rule in the set's own repository (a `root_symlinks` form crashed Bazel 9.2.0).
- **Windows batch files:** Rust std's `Command` escapes `.bat`/`.cmd` arguments and refuses unsafe ones since the CVE-2024-24576 fix; a hand-built `CreateProcessW` command line with CommandLineToArgvW quoting is unsafe for `cmd.exe`.

**Key insight:** every hard property (relocatable, network-less, shell-less, signature-stable, cross-host) is met if the launcher bytes never change and all per-tool state is a sidecar whose paths are relative to one token.

## Considered Options and trade-off matrices

Weights: 5 = binding requirement, 3 = strong driver, 1 = nice to have. Score 1-5; total = Σ weight × score.

### Choice 1 — how the persisted manifest is held against breaks

| Criterion (weight) | A. Fourth gated document `vendor-manifest` | B. Golden byte test only (like `project-lock`) | C. Pseudo report root inside `reports.json` |
|---|---|---|---|
| Fails on a break without a version bump (5) | 5 | 1 (a regenerate re-blesses the golden) | 5 |
| Own versioned URL `schemas/<name>/v<N>.json` like the other specs (5) | 5 | 5 | 1 |
| Change size in `ocx_sdkgen` (1) | 3 | 5 | 5 |
| SDK generators see a coherent document set (3) | 5 | 3 | 2 (a file posing as stdout) |
| **Total** | **68** | 44 | 41 |

**Chosen: A.** `ocx_sdkgen` gains `Kind::VendorManifest`, `Documents.vendor_manifest`, baseline `crates/ocx_schema/contract/baseline/vendor-manifest.json`, ledger `document = "vendor-manifest"`, the version read from `/properties/schema_version/const` as for the error document. Lint rules: the reports set minus the report-root rules (L09, L13, L14).

### Choice 2 — launcher sidecar format (re-scored under R-1)

| Criterion (weight) | A. NUL-delimited records | B. JSON, `serde_json`, `schema_version` | C. Data appended to the binary |
|---|---|---|---|
| No hand-rolled wire parser (5) | 2 (a bespoke grammar, however simple) | 5 | 3 |
| Launcher bytes identical for every tool, signature intact (5) | 5 | 5 | 1 |
| Within 512 KiB per target (3) | 5 (316 KiB measured) | 4 (365 KiB measured, gnu; other targets gated in CI) | 5 |
| Forward-compatible evolution (3) | 1 (any change is a new grammar) | 5 (version field, typed fields) | 2 |
| Readable by a human (1) | 2 | 5 | 1 |
| **Total** | 55 | **82** | 42 |

**Chosen: B** (R-1). The sidecar is a JSON object with `schema_version: 1`; the launcher deserialises it with `serde_json` into types that refuse unknown fields; an unknown version or field exits 78. `ocx_shim` gains `serde` and `serde_json` as dependencies; its crate-map row gains only `ocx_env_apply`. The types live once in `ocx_env_apply::direct` and both the writer and the launcher use them.

### Choice 3 — where a launcher for a platform that is not the host's comes from

| Criterion (weight) | A. All six blobs embedded in every `ocx` | B. Launcher published as an ocx package | C. Host blob only, refuse cross-platform |
|---|---|---|---|
| Linux host vendors for macOS and Windows (5) | 5 | 5 | 1 |
| Launcher and sidecar versions in lockstep (5) | 5 | 2 (the fetched launcher may be another release) | 5 |
| Offline-first, no release-order coupling (3) | 5 | 2 (dev builds must publish it too) | 5 |
| Fits the existing provenance + SHA-canary gate (3) | 5 | 3 | 5 |
| Binary size (1) | 3 (≤ 6 × 512 KiB, budget, unmeasured beyond gnu) | 5 | 5 |
| **Total** | **83** | 55 | 65 |

**Chosen: A** (DV-3). Linux blobs are static musl (`x86_64-`/`aarch64-unknown-linux-musl`), one per architecture, so they run on glibc, musl and shell-less static images. Blobs are refreshed only when `crates/ocx_shim/**` or `crates/ocx_env_apply/**` changes (≈ 2 MiB of git history per refresh); the refresh workflow is the existing provenance gate extended to six targets.

### Choice 4 — one env-apply implementation for `ocx exec` and the launcher

| Criterion (weight) | A. New leaf crate `ocx_env_apply`, both link it | B. A copy in the launcher, held by a parity test |
|---|---|---|
| No second implementation (owner rule) (5) | 5 | 1 |
| Launcher size (3) | 5 | 5 |
| Change cost (1) | 2 (15 caller files in seven crates, mechanical) | 5 |
| **Total** | **42** | 25 |

**Chosen: A.** The parity test (V-22) still runs, as the source requires in both cases.

### Choice 5 — how package files reach the directory

| Criterion (weight) | A. Copy (data and mode only), then normalise | B. Hardlink from the home's layer store | C. Reflink where available |
|---|---|---|---|
| Directory independent of a home, any device (5) | 5 | 1 (`CrossesDevices`, edits leak into the home) | 3 |
| Byte- and digest-stable across machines (5) | 5 | 3 | 4 |
| Disk and time cost (1) | 2 | 5 | 4 |
| **Total** | **52** | 25 | 39 |

**Chosen: A.** `LinkMode::{Hardlink, Copy}` on the one call site `assemble.rs:578`; existing callers keep `Hardlink`. Copy writes data and the masked mode only (no xattrs, ACLs or clone metadata; setuid, setgid, sticky and group/world-write bits cleared).

### Choice 6 — how rules_ocx's CI gets the unreleased `ocx` of the ocx pull request

| Criterion (weight) | A. `OCX_INSTALL_BINARY` + `OCX_INSTALL_BINARY_SHA256` | B. Hand-made dist manifest served by the rig | C. Relax `version`, add an OCI row type |
|---|---|---|---|
| Works for the dev build at `ghcr.io/ocx-sh/dev/cli` (OCI only) (5) | 5 | 2 (archives by hand) | 4 |
| Touches no released contract (`dist.json` schema 1) (3) | 5 | 5 | 1 |
| Serves every CI leg, macOS and Windows included (3) | 5 | 2 | 5 |
| **Total** | **55** | 31 | 38 |

**Chosen: A.** `ocx_download` copies the file through `rctx.download("file://<path>", sha256 = <hash>)`, so the bytes are verified and later edits to the source file cannot change the tool. CI pulls the dev build with a released ocx (`ocx package pull ghcr.io/ocx-sh/dev/cli:<X.Y.Z-prN>@<digest>`) and exports both variables. The extension needs the same binary to compute facts; CI legs that set the variables run with `--lockfile_mode=off`, so facts from a dev binary never reach a committed lock.

### Choice 7 — launcher root lookup (after spike 6)

| Criterion (weight) | A. Started path when it ends with `self` → `<P>.runfiles/<entry>` → `$RUNFILES_DIR/<entry>` only when P lies under it; `vendor_id` binds the sidecar | B. Source order plus a runfiles-manifest-file mode | C. Starlark writes a root hint |
|---|---|---|---|
| Links visible (they exist only in the runfiles tree) (5) | 5 | 5 | 5 |
| No Bazel term in directory or manifest (5) | 4 (the launcher binary knows the runfiles convention, DV-12) | 4 | 1 (and Starlark writes launcher data) |
| Works outside Bazel and after a move (5) | 5 | 5 | 2 |
| Not diverted by an inherited environment (3) | 5 | 4 (`RUNFILES_MANIFEST_FILE` inherits too) | 3 |
| Windows default runfiles (manifest only) (1) | 1 | 1 (a manifest has no directory for the token) | 3 |
| Implementation size (1) | 4 | 2 | 3 |
| **Total** | **90** | 85 | 55 |

**Chosen: A** (DV-6). Full algorithm: system design C-012.

## Decision Outcome

**Chosen:** ocx ships `ocx vendor` and `ocx package vendor`, the `vendor-manifest` v1 schema as a fourth gated document, a `.direct` launcher kind with a JSON sidecar built for six targets, and the leaf crate `ocx_env_apply`. rules_ocx becomes one repository rule per set and platform that runs `ocx vendor` in an isolated home, a hub generated from facts produced by `ocx vendor --manifest-only`, and a Buildbarn + NativeLink rig in its CI.

### What a run writes (layout internal; the manifest names every path)

```
DIR/manifest.json
DIR/packages/<registry>/<algorithm>/<2 hex>/<30 hex>/content/…                one per package digest, once
DIR/packages/<registry>/<algorithm>/<2 hex>/<30 hex>/entrypoints/<name>[.exe] + <name>.direct
DIR/selections/default/shells/default/bin/<name>[.exe] + <name>.direct       one per tool of the selection
```

Every launcher is a copy of the target's blob (no hardlinks inside `DIR`). Nothing under `refs/`, no `metadata.json`, `resolve.json`, `install.json`, `digest` or OCI `manifest.json` per package (DV-1). No `active` link and no `links/<group>/<entry>` tree (DV-2). The whole tree is staged in a fresh private directory beside `DIR` and renamed into place (R-8).

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
VendorManifest { schema_version: 1, content: present|absent, platform, available_platforms[], vendor_id, dir_token: "@VENDOR_DIR@",
                 packages[ {identifier, digest, package_dir, content_dir, dependencies[], names_declared,
                            entrypoints[{name, launcher_path, environment}],
                            tree?{files[{path,executable}], links[{path,target}], link_free_dirs[]}} ],
                 environments[ {id, scope{type: selection|entrypoint, …}, items[{key, value, kind, separator?}]} ],
                 selections[ {id, groups[], bindings[], roots[], environment, launcher_dir} ],
                 tools[ {name, selection, kind: binary|entrypoint, package, environment, target_path, launcher_path?} ] }
```

**Specified for later, not in v1 (R-5):** the native download (`rctx.download`) needs, per package, the ordered layer list `{digest, size, media_type}` and a physical blob URL after index and mirror resolution. It lands as an additive optional field `packages[].layers` when that consumer is built; `tree.empty_dirs` lands the same way when a consumer needs it.

### Quantified impact

| Metric | Before (0.5.1, spikes) | After (target) | Notes |
|---|---|---|---|
| Build on network-less executors | fails (launchers re-enter `ocx`) | passes | V-01 |
| Fresh client, warm remote cache, repo contents cache on | rule runs, 6 req / 39.1 MB | 0 req, ~344 KB read | spike shape; V-03 re-measures with `ocx vendor` |
| `ocx` in an action's inputs | always | never | V-06 |
| Cold fetch of one tool (node) | 3.0 s rule run | ≤ 1.2 × pull time | budget; measured by `rig:baseline` |
| Per-`ocx` binary size | Windows: one blob | + ≤ 6 × 512 KiB | budget, asserted per blob; measured only for gnu (365 KiB) |
| Facts in `MODULE.bazel.lock` | none | ≈ 17 KB lock text + names per project set | R-6; spike 5 measured 13 KB of richer facts for 20 tools × 4 platforms |

### Consequences

**Positive:** one interface for Bazel and later consumers (shell files, installer renderers) with no ocx internals in Starlark; network-less, shell-less executors work; the whole set travels with every tool; a vendored directory is the movable install the owner missed in import.

**Negative:** a second copy of every package on disk; the set repository keeps no layer cache between fetches unless a blob cache or blob directory is given (R-4); every action using a tool of a set keys on the whole set, so rules_ocx's docs recommend one `ocx.project` tag per group that a target needs, the caller's lever on re-keying; the vendored launcher writes no `__OCX_ENV_STATE`, so an `ocx exec --isolate` nested under a vendored tool cannot revert the vendored values (it warns, `CannotIsolate`); rules_ocx's next release breaks every `bins` user and drops Bazel 8.

**Risks and mitigations:**

| Risk | Mitigation |
|---|---|
| A Bazel source-directory label behaves differently from file labels on a backend | V-34 compares input lists on both backends |
| Repo contents cache skips repositories that keep symlinks (issue 31456) | links always off disk in rules_ocx (`--omit-links`, the tested form) |
| Facts go stale | the fact carries the full `ocx.lock` text, groups, names and platform list; any difference recomputes (R-6, C-RO-10); the set repository cross-checks names, kinds and platforms (C-RO-12) |
| Cross-host rendering differs (Windows separators, `:native`) | vendor lane renders `/` after the token; `:native` renders the target separator; V-46 compares Linux-host and Windows-host manifests |
| A shared remote repo contents cache is written by an untrusted client | docs: only trusted CI lanes write it; content is fetched into an isolated home from digest-verified blobs (R-4) |
| Unsigned darwin Mach-O content vendored on Linux stays unsigned | stated limit; all 12 sampled Mach-O files carry signatures; ocx signs only on a macOS host |
| Windows default runfiles have no tree | launcher exits 78 naming `--enable_runfiles`; rules_ocx docs require it on Windows (DV-6, owner confirmation deferred) |

## Orchestrator rulings (goal-loop authority)

| ID | Ruling | Reason |
|---|---|---|
| R-1 | The `.direct` sidecar is JSON, parsed by `serde_json`, versioned by `schema_version`; an unknown version or field exits 78 (Choice 2) | measured 365 KiB vs 316 KiB against a 512 KiB budget; removes a hand-rolled wire format (quality-core, Block) and gives a version field |
| R-2 | V-23, V-31 and V-37 are "review/measure, no red" rows | the source itself defines them as review checks |
| R-3 | The launcher keeps `ocx exec` parity: it strips `Propagation::{Credential, HookLocal, State}` and passes `SecretInput` through. **Owner-revisitable.** | V-22's parity check; a tool may itself call ocx with registry auth |
| R-4 | The set repository rule always runs ocx with an isolated scratch `OCX_HOME` inside the repository directory, deleted before return; content comes from blobs verified by digest in that run, or from a digest-checked blob directory or cache; the client's own home is never read; only trusted lanes write a shared remote repo contents cache | the source's recommended home; one tampered home must not poison every client of a shared cache |
| R-5 | v1 ships without `packages[].layers` and `tree.empty_dirs` (specified above as later additive fields); `links_on_disk` is cut, `--omit-links` stays; the generator version is only in `VendorReport`; both rig backends stay, NativeLink may be its own job of the same workflow but must be green once | the one-way door carries only what a consumer reads now; the source's Verification requires two backends |
| R-6 | Facts identity is exact: the value carries the full `ocx.lock` text, the tag's groups and names, and the platform list; on a hit the extension compares them with `module_ctx.read` of the lock and the tag; any difference recomputes. `hash()` + length is the key only | Starlark has no sha256; a 32-bit key alone cannot prove identity |
| R-7 | No host leaf is assumed: `platforms` is a required attribute; `--manifest-only` works for any `-p` the lock pins; the manifest's `available_platforms` is computed from the lock (or the image indexes) before leaf selection, and a listed platform outside it fails naming the platform | spike 5: a five-group set resolves on 4 of 10 lock keys; a Linux client may declare a Windows-only set |
| R-8 | The whole tree is staged in a fresh `mkdtemp` (mode 0700, random name) in `DIR`'s parent and renamed into place (onto an absent or empty `DIR`); a failure leaves `DIR` as it was; every temp name is random; no write follows a symlink | interrupted runs left a non-empty `DIR` (codex) and fixed temp names invited TOCTOU and symlink redirection (SEC-W6) |

## Open-question decisions

Every entry of the source's `## Open questions` (lines 627-720), in source order.

| ID | Question | Decision | Reason |
|---|---|---|---|
| OQ-01 | Mach-O bytes of macOS ad-hoc signing identical across machines? | Not needed for the launcher (committed bytes, never re-signed); for content, macOS CI pulls node, shfmt and python-build-standalone darwin/arm64 with and without `OCX_NO_CODESIGN` and compares every sha256 (V-27b) | ocx re-signs only Mach-O whose signature fails; all sampled ones verify |
| OQ-02 | Which backends keep symlink nodes in an input root? | Buildbarn hardlinking worker and NativeLink 1.9 in the rig (V-12) | run on 2026-11-25; hosted services unchecked and named so in the docs |
| OQ-03 | How does the launcher find its trees inside an action? | Choice 7 A; algorithm C-012 | links live only in the runfiles tree; spike 6 on Linux; Windows is V-45 |
| OQ-04 | Is the unpacked repository identical across machines? | Yes after normalisation C-008; V-09 compares input digests from two machines | umask and clock are the only differences measured |
| OQ-05 | Does `package export` fetch dependency blobs per dependent? | Falls away: the rule does not run export | ratified |
| OQ-06 | What home does the repository rule give `ocx`? | Always an isolated scratch `OCX_HOME` inside the repository directory, deleted before return; `HOME` stays the client's, so Docker configuration and credential helpers work; an optional blob cache or blob directories (digest-checked) avoid refetching (R-4) | the client's ocx home is never read, so it cannot poison a cache entry |
| OQ-07 | Fresh client with warm remote cache, repo contents cache on/off? | Measured (spike S-B): on 0 req / ~344 KB, off 6 req / 39.1 MB; docs recommend the flag from 9.3.0; V-03 re-runs with `ocx vendor` | the spike measured the cache, not ocx |
| OQ-08 | How does a tool find an entry point of another package by bare name? | The composed `PATH` already holds each package's `entrypoints/` directory (`composer.rs:400`), where the vendor writes direct launchers | no extra directory, no new composition (V-30) |
| OQ-09 | Package content holding `BUILD`, `BUILD.bazel`, `MODULE.bazel`, `REPO.bazel`, `WORKSPACE*` | ocx writes it unchanged and lists it in `tree.files`; rules_ocx moves each such file (matched case-insensitively) to a numbered path under its own `_ocx_reserved/` directory and declares it back at its place as an output (DV-4, C-RO-05) | a renaming flag in ocx is a Bazel term in the interface; numbered paths cannot collide with package files |
| OQ-10 | Empty directories kept? | ocx writes them on disk and does not list them in v1 (R-5); rules_ocx declares none; a rig fixture records what a directory label does with one | fidelity for non-Bazel consumers; 2 of 69 packages have one |
| OQ-11 | What goes into facts, how large? | One fact per set: the full `ocx.lock` text (project tier), groups, names, `platforms`, `available_platforms`, and per platform `schema_version` and `[name, kind]` per tool (R-6); a digest-pinned package needs no lock text | ≈ 17 KB lock + names for this repository (16981-byte lock) |
| OQ-12 | Composed environment a fact or joined in Starlark? | Neither: ocx composes it into the manifest | ratified |
| OQ-13 | Command family name and verbs | `ocx vendor`, `ocx package vendor`, flag `--manifest-only` | ratified |
| OQ-14 | Manifest with no files written | `--manifest-only` writes `DIR/manifest.json` alone, `content: "absent"` | ruled 2026-11-26 |
| OQ-15 | Default for `-o` | none, required | ruled |
| OQ-16 | Target exists and is not empty | refused before any network: 64 `vendor_output_not_empty` (a symlink at `DIR` counts as not a directory); 74 on I/O; `DIR` unchanged | the `package test --output` rule; R-8 keeps a failed run from leaving a non-empty `DIR` |
| OQ-17 | Manifest to stdout? | No | ruled |
| OQ-18 | Manifest location | `DIR/manifest.json` | ruled |
| OQ-19 | How is the file held against breaks? | Choice 1 A | the gate must fail on a break |
| OQ-20 | What does the command print? | `VendorReport` v1: `{schema_version, ocx_version, output_dir, manifest_path, content, platform, packages[{identifier, digest}], tool_count, written_size}` | as `ExportReport`; the generator version lives here only (R-5) |
| OQ-21 | Second executable? | No | ruled |
| OQ-22 | Directory labels without walking, in non-Bazel terms | `tree.links` with targets, `tree.link_free_dirs` (top-most directories with no link at or below), plus `tree.files` (DV-5) | plain facts about a tree |
| OQ-23 | Identity fields per package | `identifier` (pinned; tag when one was given), `digest`, `dependencies`; descriptive fields come from `ocx package description pull` | the first release needs none of the descriptive fields (system design § 11) |
| OQ-24 | Shell files written or printed? | Deferred | allowed by the owner |
| OQ-25 | More than one selection per run? | No; `selections`, `environments` and `tools` are lists | ruled |
| OQ-26 | Env application without drift | Choice 4 A | owner's reuse rule |
| OQ-27 | Variables `ocx exec` removes or writes | Sidecar `unset` = `ocx_env::keys_with(Credential, HookLocal, State)` plus `OCX_BINARY_PIN`; `SecretInput` passes through (R-3); the launcher writes neither `__OCX_ENV_STATE` nor `OCX_BINARY_PIN`; mode Layer (on top of the caller's environment), `ocx exec`'s default | one source for the list; no `ocx` to pin |
| OQ-28 | Copy or hardlink | Choice 5 A | independence from the home |
| OQ-29 | Kept package files | `content/` and the package's `entrypoints/` launchers with sidecars (DV-1) | everything else holds absolute paths or home state |
| OQ-30 | Package declaring neither binaries nor entry points | `NotEnumerablePolicy::Skip`: no tools, still in `packages` and the environment; `names_declared: false` in both modes (V-53) | the toolchain renderer's rule; names from metadata only; spike 5 found 0 of 20 such tools here |
| OQ-31 | Patches and client configuration | Patches never reach a vendored environment, whatever `[patches]`, `OCX_PATCHES` or `OCX_PATCH_SNAPSHOT` say; configuration that only routes bytes (mirrors, registries, auth) and the trust policy apply, in both modes (V-63) | a vendor is a render and renders do not model companions; routing is digest-verified |
| OQ-32 | Signature on every path on Apple Silicon | ocx CI on `macos-latest`: hardlink under another name, copy, rename, tar round trip, a quarantined copy; control: one changed byte is killed (V-27); rules_ocx macOS CI: the launcher runs after `bazel vendor` and from the disk cache (V-27c); no macOS remote executor exists (DV-13) | no macOS host here |
| OQ-33 | Launcher for a non-host platform | Choice 3 A | offline-first, lockstep |
| OQ-34 | Installed-home launchers move now? | Later, under [ocx-sh/ocx#679](https://github.com/ocx-sh/ocx/issues/679); on Unix the program accepts only `.direct` (others exit 78, V-52) | ruled |
| OQ-35 | darwin content needing signing, vendored on Linux | Stated limit | ocx signs only on a macOS host |
| OQ-36 | Marker for the directory in a value | The literal token `@VENDOR_DIR@`, replaced by plain search; a resolved value that already contains it, contains NUL, is not UTF-8, or still holds the home's or `DIR`'s absolute path after replacement is refused (65 `vendor_value_unrepresentable`) | no escape syntax exists, so none can be got wrong |
| OQ-37 | Does `inspect` need an addition? | No | ratified |
| OQ-38 | Declared symlinks on a Windows executor | Not in the first release: a `windows/*` set whose manifest lists links `fail()`s naming the package (V-49) | no Windows package with links is known |
| OQ-39 | Platform string | The full canonical string, from the required `platforms` attribute, passed verbatim; `available_platforms` validates it (R-7) | `-p linux/amd64` fails for libc-only publishers |
| OQ-40 | Repositories of unused platforms fetched? | Hub holds `toolchain()` declarations only; it names set repositories only inside `toolchain()` | spike 5 |
| OQ-41 | `bazel run` export target | Dropped | `bazel vendor` and `blob_dirs` cover offline |
| OQ-42 | Credential sources inside a repository rule | The client's Docker and ocx configuration files and the inherited process environment only; neither the extension nor the rule calls `getenv` on a credential or secret variable; nothing secret in `MODULE.bazel.lock`, a repository, a marker file, an action key or an execution log (V-10) | `getenv` records values in marker files and the lock |
| OQ-43 | Physical blob URL in `package inspect` | Not added now; specified above as part of the later `packages[].layers` field | native download out of the first release |
| OQ-44 | Supported Bazel 9 releases | All 9.x; `bazel_compatibility = [">=9.0.0"]`; the cache flag recommended from 9.3.0 | 9.2.0 did not hang in the spike |
| OQ-45 | Content-tree command for the first release? | Falls away: `ocx vendor` is it | ratified |
| OQ-46 | Conformance test shipped or rig-only? | Rig-only (V-12) | a broken backend fails loudly on the first package with links |

`[NEEDS CLARIFICATION: Does NativeLink admit an arbitrary instance-name suffix as a per-test cache silo, as Buildbarn does?]` Default if not: a per-test `--action_env=OCX_RIG_SILO=<key>` salt plus a leg with the repo contents cache off; decided in the rig, does not change ocx.

`[NEEDS CLARIFICATION: Does a Bazel source-directory label keep an empty directory and a nested BUILD file below it on both backends?]` Default: treat neither as kept (OQ-09, OQ-10 route around both); a rig fixture answers it.

## Divergences from the source

| ID | Source says | This ADR | Strong reason |
|---|---|---|---|
| DV-1 | Whether `metadata.json`, `manifest.json`, `resolve.json`, `install.json`, `digest` go along is the ADR's call | none of them | they describe a home; `refs/` is absolute |
| DV-2 | Selection directory "as in a rendered toolchain home (`links/<group>/<entry>`, `shells/`)" | `shells/default/bin/` only | `active` and `links/` alias package roots the manifest names; additive later |
| DV-3 | Non-host launcher "published per platform as an ocx package" (recommended) | embedded (Choice 3) | lockstep with the sidecar, offline-first |
| DV-4 | ocx "stores such a file under another name on request" | rules_ocx moves it after the run | the flag would be a Bazel term in ocx's interface |
| DV-5 | "a list of every file makes the document large" | `tree.files` added | reserved names detectable with no walk; installers need file lists; size stays out of facts |
| DV-6 | Lookup: started path, `.runfiles`, then the runfiles manifest | started path, `<P>.runfiles`, `$RUNFILES_DIR` only when P lies under it; no manifest-file mode; Windows needs `--enable_runfiles` (owner confirmation deferred) | a manifest-only tree has no directory to stand for the token |
| DV-7 | rules_ocx runs the command in a repository rule, extension untouched | the extension also runs `ocx vendor --manifest-only`; invariant 3 amended | facts need it |
| DV-8 | Patches: "the same composer brings patches in when the client configures them" (recommended) | never (OQ-31) | a vendored directory must not depend on site overlays; `patch_snapshot` goes |
| DV-9 | V-16: "a digest and its blob data as plain attributes" | digest as a plain attribute; blob data specified for a later field | the native download is out of the first release |
| DV-10 | V-17: "This repository builds with the new rules_ocx" | shown with `--override_module` in the run; the committed move waits for the rules_ocx release | the stated release order |
| DV-11 | Owner's grammar lists `-o`, `-g`, `NAME`, `--manifest-only` | adds `-p` and `--omit-links` | the ratified proposal's "one platform" and "on request it leaves the links off the disk" |
| DV-12 | "Nothing in the directory … is specific to Bazel" | the launcher binary inside `DIR` knows the runfiles convention (`.runfiles`, `RUNFILES_DIR`); the manifest, sidecars and every other file do not; V-35 scans all but launcher bytes | the source itself has the program find the directory "through Bazel's runfiles" |
| DV-13 | Signature paths include "Bazel's upload to a remote executor" | not checked: no macOS remote executor exists in the rig or CI | the other paths are V-27 and V-27c |

## Component contracts (summary; numbered in full in the system design)

1. **`ocx vendor`** (C-001…C-011, C-021…C-024): 64 without `-o`, writes nothing; refuses a non-empty `DIR` (64) before any network; stages the whole tree privately and renames it into place, so a failure leaves `DIR` as it was; `--manifest-only` writes one file and fetches no layer; both modes give equal manifests except `content` and `tree`; environments equal `ocx env --pinned` with package paths under the token and patches absent; files equal the installed `content/` in bytes, executable bit and link target; dirs 0755, dir and link mtimes 315532800; manifest bytes do not depend on host OS, user, umask, clock or `OCX_HOME`; `available_platforms` is the lock's (or indexes') intersection, computed before leaf selection.
2. **`.direct` launcher** (C-012…C-020): root lookup with own-image identity, bounded regular-file reads and `vendor_id` binding; root made absolute lexically; `unset`, then `path`/`list`/`constant` through `ocx_env_apply`; `program` structurally `@VENDOR_DIR@/<relpath>`; `.bat`/`.cmd` targets spawned through std `Command`; Unix `execve`, Windows `CreateProcessW` + job object; writes nothing.
3. **rules_ocx** (C-RO-01…C-RO-17): runs no ocx command other than the two vendor commands; reads no ocx file other than `manifest.json`; isolated home; one repository per set and platform; hub from exact facts; guard script red on one planted violation.

## NFRs

- **Scalability:** manifest ≈ 60 B per file (node: 4800 files ≈ 300 KB; a 100k-file fixture is budgeted at ≤ 10 s for `json.decode` plus BUILD generation, V-65), in the repository's file only; facts ≈ one lock text plus ≈ 30 B per tool name per platform per set; the remote cache stores each file once.
- **Availability:** no network needed with `--blob-dir`, `bazel vendor` or a warm repo contents cache; after `bazel clean --expunge` with the registry down, only `bazel vendor`, `blob_dirs` or the remote repo contents cache refetch a set; the launcher depends on no service and no `ocx`.
- **Latency:** `--manifest-only` ≤ `inspect --resolve --closure` (0.7-1.9 s measured); a vendor run ≤ 1.2 × the pull time of the same set; launcher overhead ≤ 5 ms to `execve` on Linux with ≤ 1000 runfiles entries (measured, V-66).
- **Security:** the child gets exactly what `ocx exec` gives it: `Credential`, `HookLocal` and `State` variables are stripped and `SecretInput` variables pass through (R-3, owner-revisitable); a sidecar holds no value of a secret by construction (keys only); `program` and `self` are structurally inside the root; `.bat`/`.cmd` targets get std's batch-argument escaping or a refusal; sidecar reads are bounded, regular-file only; the launcher's own path must be its running image; launcher bytes immutable (an ad-hoc macOS signature gives integrity, not authenticity; a quarantined copy is blocked by Gatekeeper until the attribute is removed); the set rule fetches into an isolated home from digest-verified blobs; only trusted lanes write a shared remote repo contents cache; no secret in facts, a repository, a marker file, an action key or an execution log (V-10).
- **Cost:** ≤ 3 MiB budget per `ocx` binary; ≈ 2 MiB git history per blob refresh; the first remote upload is the unpacked size (144 MB vs a 59 MB archive for node); rig CI ≈ 10 min per backend job on one Linux runner.
- **Operability:** every ocx failure maps to an existing exit code with an `error.detail` slug; rules_ocx maps every reachable sysexit to a `fail()` hint (its invariant 4); the rig's nginx JSON log witnesses registry bytes.

## C4 sketch

**Context.** A developer or CI runs Bazel on a client. The client reaches an OCI registry or mirror (auth via the client's Docker/ocx config) and a REAPI backend. Executors reach nothing. ocx runs on the client only.

**Containers.** On the client: Bazel; the rules_ocx module extension (runs `ocx vendor --manifest-only`, writes facts to `MODULE.bazel.lock`); the hub repository (toolchain declarations + tool targets); one set repository per set and platform (runs `ocx vendor` in an isolated home, holds the vendored directory); `@ocx_tool`. On executors: the uploaded set files, declared symlinks and the native launcher.

**Components (ocx).** `ocx_cli::command::{vendor, package_vendor}` → `ocx_package_manager::tasks::vendor` → existing `select_tool_set`/`resolve_selected_tools`, `compose_roots` with `PathLane::Vendor`, `toolchain_surface` + `exposed_names`, `pull` with `dest_override` + `LinkMode::Copy`, `render_toolchain`'s direct target kind and `launcher::generate`'s direct kind → `ocx_package::vendor::VendorManifest` → `ocx_schema` → `ocx_sdkgen` gate. `ocx_shim` + `ocx_env_apply` form the launcher; `ocx_store::shim` embeds six blobs.

## Validation

- [ ] Every row of the system design § 9 shown red once and green once, except the rows marked "review/measure, no red"
- [ ] Security review of the launcher (lookup, env removal, Windows spawn, batch targets) by the security-auditor skill
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
| 2026-11-26 | architect worker | Review round 1: R-1…R-8 recorded; JSON sidecar; isolated home; staged publish; exact facts; explicit platforms; Windows batch spawn; matrices re-computed; DV-12, DV-13 |
