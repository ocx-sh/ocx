# ADR: Package bundles (`ocx package export` / `import`) and the Bazel fetch plan

## Metadata

**Status:** Proposed (revised after review round 1)
**Date:** 2026-11-08
**Deciders:** owner (ratified decisions in the dossier), architect worker (`/hex-architect` xhigh)
**Reversibility:** two separate one-way doors, both high.

1. **Bundle format.** Bundles outlive the binary that wrote them, so every later ocx must import them. The format is versioned by `sh.ocx.bundle.version`.
2. **The `fetch_plan` report.** Once rules_ocx consumes it, its segment grammar, its package-key scheme, its `bundle_files` and its `bundle_path` layout become a persisted contract. A change then needs a coordinated rules_ocx release, not only a ledger entry.

The CLI grammar and the three new report roots (`package_export`, `package_import`, `fetch_plan`) are interfaces under `adr_ocx_interface_contract.md`.

**Blast radius:** Cross-repo, external contract. It reaches `ocx` (seven crates plus the CLI), `rules_ocx` (store model, launchers, extension attributes) and every published bundle. oras, skopeo and regctl read the same directory.
**Domain tags:** integration, data, security, devops
**Tech strategy alignment:** Golden path. Rust 2024 and Tokio, no new infrastructure. One dependency decision is open: a directory-capability library for traversal-safe bundle reads (`ocidir`, `cap-std` or `rustix`). It is settled by the `deps` skill in WP-1 (system design § 10).
**Required artifacts:**
- this ADR;
- `.claude/artifacts/system_design_package_export_import.md` (contracts, C4, schemas, STRIDE, phases, test map, deferred findings);
- research, persisted:
  - `.claude/artifacts/research_bazel_downloader_oci_blobs.md`
  - `.claude/artifacts/research_package_bundle_recon.md`
  - `.claude/artifacts/research_package_bundle_prior_art.md`
  - `.claude/artifacts/research_package_bundle_oci_layout.md`
  - `.claude/artifacts/research_package_bundle_competitive.md`
  - `.claude/artifacts/research_package_bundle_archaeology.md`
  - `.claude/artifacts/research_package_bundle_distro.md`
  - `.claude/artifacts/research_package_bundle_launcher_bytes.md`

**Input:** `.agents/discussions/bazel-hermetic-package-bundles.md`. Its Decisions are honoured except where § "Changed or unhonoured owner decisions" names a change.

## Context

Moving packages between machines today has two routes, and both fall short.

- **Air-gapped transfer** works only through a registry mirror ([`guides/work-offline.mdx`](../../website/src/content/docs/guides/work-offline.mdx), Differentiator #9).
  - The documented relocatable home subset (`blobs/`, `layers/`, `index/`) cannot be re-verified. The home keeps no compressed layer blobs, and the extracted trees carry no digest a receiver could check (`research_package_bundle_archaeology.md` § Relocatability).
  - [ocx-sh/ocx#23](https://github.com/ocx-sh/ocx/issues/23) was closed with "requires dedicated import/export command".
- **Bazel** (`rules_ocx`) fetches into a shared absolute `$OCX_HOME` and symlinks `content/` out of it (`rules_ocx/ocx/private/package.bzl:255-257`).
  - Its launchers bake `OCX_HOME`, `OCX_BINARY_PIN` and absolute store paths (`rules_ocx/ocx/private/repo_utils.bzl:1516,1540`).
  - So launcher bytes, and any action that takes a launcher as input, differ between machines. This was measured: `research_package_bundle_launcher_bytes.md` § Measurement.

## Decision Drivers

1. **Content integrity.** Every byte entering a store is verified against an OCI digest before use, by ocx and by Bazel.
2. **The local index is the package-tier lock** (`subsystem-oci.md` § load-bearing). A pin moves only under a command that names what it moves. A routing pointer is remote-authored data, and a local answer wins (`.claude/rules/subsystem-oci.md:681-682`).
3. **Reuse over new code.** The pull pipeline already enforces caps, completeness, digest, codesign and extraction guards (`crates/ocx_oci/src/client.rs:862-1019`, `crates/ocx_util/src/archive/tar.rs:223-305`). The registry copy already carries referrers and cosign sidecar tags (`crates/ocx_oci/src/copy.rs:172,236,418`).
4. **Tier purity.** An OCI-tier `ocx package …` command never reads `ocx.toml` (`subsystem-cli.md` § Layering).
5. **rules_ocx invariant 1.** Starlark holds no OCI, auth, store or assembly knowledge (`rules_ocx/AGENTS.md`). The CLI's JSON is the only parse surface.
6. **Hermetic Bazel.** Action keys must be identical across `OCX_HOME`, output base and machine. The fetch goes through the repository cache, `--downloader_config` and `bazel vendor`.
7. **Interop (oras-go family).** A bundle is a valid OCI image layout, so `oras cp --from-oci-layout` can seed a registry with it. skopeo is claimed only if spike S3 passes.

## Research summary

- **Prior art.**
  - oras, skopeo and regctl copy to and from OCI layouts. pixi-pack ships a lockfile plus packages.
  - Nix (`nix copy --to file://`) and `docker save` export content-addressed stores.
  - rules_oci `oci_pull` downloads each blob with `rctx.download` and never extracts.
  - rules_img fetches the manifest and config only, and fetches layers lazily.
  - No ruleset extracts multi-layer OCI content natively in a repository rule (`research_bazel_downloader_oci_blobs.md:57`).
- **Layout semantics** (`research_package_bundle_oci_layout.md`).
  - Extra files are legal, and `ref.name` has no strict grammar.
  - Referrers in a layout follow four incompatible conventions. Image-spec #1171 is a draft. Interop is therefore claimed for the oras-go family only: untagged descriptors, discovered by `subject`.
- **Bazel downloader** (`research_bazel_downloader_oci_blobs.md`).
  - `download(url, sha256=<hex>, canonical_id=<digest>)` fits an OCI blob, and the checksum doubles as the repository-cache key.
  - The downloader cannot answer a bearer challenge, so credentials come from a credential helper.
  - [bazelbuild/bazel#29660](https://github.com/bazelbuild/bazel/issues/29660) reports `Authorization` forwarded across cross-host redirects. Commit 4f13ff7 (2026-07-27) reportedly drops it on a cross-origin hop and re-adds credential-helper headers for the new URL. Which release carries it is UNVERIFIED, and spike S1 settles it.
- **Distro renderers** are deferred. Their input needs are recorded in `research_package_bundle_distro.md`; they need description, entrypoints and env.

## Considered options

### Bundle format

Weights: integrity 30, interop 20, reuse 20, import simplicity 15, transport ergonomics 10, size 5. Each cell is score (weighted).

| Option | Integrity | Interop | Reuse | Import | Transport | Size | Weighted |
|---|---|---|---|---|---|---|---|
| **A. OCI image layout + index snapshot beside it** | 5 (30) | 5 (20) | 4 (16) | 4 (12) | 3 (6) | 4 (4) | **88** |
| B. Tarball of the home subset | 1 (6) | 1 (4) | 3 (12) | 5 (15) | 4 (8) | 2 (2) | 47 |
| C. Custom single-file ocx archive | 4 (24) | 1 (4) | 2 (8) | 3 (9) | 5 (10) | 4 (4) | 59 |
| D. `oci-archive` (tarred layout) as the primary form | 5 (30) | 4 (16) | 3 (12) | 3 (9) | 5 (10) | 4 (4) | 81 |

- **B** ships extracted layers with no checkable digest (driver 1), so it is rejected.
- **C** is a format no other tool reads.
- **D** is A plus `tar`. Users get it with `tar cf`, so ocx owns no archive writer.
- **Risk of A.** A partial layout confuses third-party tools: `oras cp` of an image index whose child is absent fails. Interop is guaranteed for complete exports and per-leaf copies only.

**Rejected: home alignment.** Making `$OCX_HOME` itself an OCI layout is rejected, with no follow-up issue.

- It reopens `adr_three_tier_cas_storage.md` D11 (`:497`).
- Home blobs are registry-namespaced.
- The home keeps no compressed layers by design.

### Bazel materialiser (re-scored in round 1)

Weights: hermetic action keys 20, cache/vendor/`--downloader_config` 15, extraction safety 20, fidelity to ocx assembly 15, invariant 1 15, auth 5, offline 10.

| Option | Keys | Cache | Safety | Fidelity | Inv. 1 | Auth | Offline | Weighted |
|---|---|---|---|---|---|---|---|---|
| A. Status quo: shared `$OCX_HOME`, ocx pulls | 1 (4) | 1 (3) | 5 (20) | 5 (15) | 5 (15) | 5 (5) | 3 (6) | 68 |
| B. Plan, then native `download_and_extract` per layer | 5 (20) | 5 (15) | 2 (8) | 2 (6) | 2 (6) | 2 (2) | 5 (10) | 67 |
| **C. Plan, then `rctx.download` of every blob, then `ocx package import` into a repository-local home** | 5 (20) | 3 (9) | 5 (20) | 5 (15) | 4 (12) | 2 (2) | 5 (10) | **88** |
| D. Compressed layers kept, ocx materialises inside each action | 3 (12) | 4 (12) | 5 (20) | 5 (15) | 4 (12) | 2 (2) | 4 (8) | 81 |

**Why B scores low.**

- Bazel's tar extractor runs without ocx's guards: entry escape, symlink ancestors, hardlink escape, setuid stripping and the post-extraction symlink sweep (`crates/ocx_util/src/archive/tar.rs:223-305`).
- B also makes Starlark own strip, prefix, overlap, whiteout and codesign rules, which breaks invariant 1.

**Why C scores high.**

- C's integrity and assembly are ocx's own: the same pull path and the same codesign.
- Starlark writes only opaque files the plan names. It parses no OCI.

**C's open score is cache/vendor (3).**

- The repository cache and `--downloader_config` work, because Bazel's downloader fetches every byte.
- `bazel vendor` stores repository trees. The repository-local home holds ocx's absolute `refs/` symlinks (`.claude/rules/subsystem-file-structure.md` § ReferenceManager). Whether vendor rewrites, keeps or rejects them is UNVERIFIED. Spike S2b settles it.
- rules_ocx exposes only `packages/…/content` (relative symlinks only, since the extractor refuses absolute targets) plus its own launchers, so the `refs/` links are never build inputs.
- If S2b shows vendor breaks, C still works for every other driver. The contingency is a NEW materialise-only import mode that writes content trees through `pull_local`'s `dest_override` with no `refs/` (`crates/ocx_package_manager/src/tasks/pull_local.rs:68,80`).

**Sensitivity.** Even with every S2a check green (safety and fidelity at 4), B scores 81, below C. B is therefore never the default. It is allowed only as a measured fetch-time optimisation, after S2a passes both its benign and its hostile half on Linux, macOS and Windows.

## Decision

### 1. Command grammar

```
ocx package export [-p PLATFORM] [--referrers | --no-referrers] -o DIR ID...
ocx package import [--verify | --no-verify] DIR [ID...]
ocx package inspect --fetch-plan [-p PLATFORM] ID@DIGEST...
ocx inspect --fetch-plan [-p PLATFORM] [-g GROUP]... [NAME...]
```

**Export.**

- **Platform rule.** For a tag, `-p` defaults to the host. For a digest identifier, `-p` is required (64): it selects the child of an image index, or declares a leaf's platform. This mirrors `package copy`'s rule for a digest source (`crates/ocx_cli/src/command/package_copy.rs:32-39`).
- **Appending.** Exporting again into the same `DIR` appends a platform (owner decision).
- **Output.** `-o` is required (IC-12).
- **Closure.** Each `ID` exports with its dependency closure.
- **Referrers.** `--referrers`/`--no-referrers` is the `package copy` pair (`crates/ocx_cli/src/options/referrers.rs:7-31`), on by default.

**Import.**

- `--verify`/`--no-verify` is `options::SignatureVerify`, the install pair (`crates/ocx_cli/src/options/verify.rs:32`, `crates/ocx_cli/src/command/install.rs:32`).
- The optional `ID...` names the packages whose tag pins import adopts (Decision 3).

**Fetch plan.** The OCI tier accepts digest identifiers only, and a tag is 64. A tag answer depends on the machine's local pin. The project tier is driven by the lock. No root `ocx export`, no `ocx index` verb.

### 2. Bundle on-disk contract

```
DIR/
  oci-layout               {"imageLayoutVersion":"1.0.0"}
  index.json               OCI image index; annotation sh.ocx.bundle.version = "1"
  blobs/<alg>/<hex>        every manifest, config, layer, referrer and description blob
  index/<registry>/…       optional pin snapshot, existing --index grammar, exported IDs only
  trust/trusted_root.json  optional, advisory; never read by import
```

**`index.json` is the membership record and the commit point.** It lists:

- **Each exported leaf (a platform).** The descriptor carries a `platform` and a digest-form `ref.name`, `<registry>/<repo>@<leaf digest>`. A leaf is in the bundle exactly when it is listed. Absence needs no marker (owner decision).
- **Each tagged export's image index**, with `ref.name` = `<registry>/<repo>:<tag>`.
- **Referrer manifests**, untagged (the oras-go convention).
- **Cosign sidecar tags** (`sha256-<hex>.sig`/`.att`/`.sbom`). `package copy` already carries these (`crates/ocx_oci/src/copy.rs:236`).
- **The repository description** (`__ocx.desc`), with its readme and logo blobs (dossier requirement: descriptions).

**What import reads.**

- Import reads `ref.name` only in its digest form, to name a listed leaf's repository.
- It also reads the sidecar-tag form, to answer a cosign sidecar lookup during verification (`crates/ocx_sign/src/verify/simplesigning_read.rs:117`). That lookup cannot splice a signature onto another subject: `check_claim` binds the signed digest (`:527`).
- Tag pins come only from the snapshot, never from `ref.name`.

**Completeness and atomicity.**

- A listed leaf with any missing blob, or a missing dependency leaf, is incomplete, and import exits 65. Import is the completeness check, so a half-copied bundle fails rather than importing partially.
- Export writes blobs, then snapshot entries, then renames `index.json` last. A crash leaves the previous `index.json` valid. New blobs and snapshot entries stay inert until a leaf that uses them is listed.

**Version.** `sh.ocx.bundle.version` fails closed when unknown (65).

- Its semantics are reserved for optional bundle signing later.
- A bundle is written only by ocx. A third-party writer that rewrites `index.json` (for example `oras cp --to-oci-layout`) may strip the annotation, and the result then imports as 65. A test records which tools strip it.

### 3. Import contract: content first, pins only when named

**Content.** Import always copies every listed leaf and its closure into `blobs/` and `layers/` through the pull path.

- It uses a new sealed `LayoutTransport` and an index with no routing roots, no authoritative source and no index namespace. Its only source is the layout-backed `OciIndex`, which keeps the default `FallThrough` jurisdiction.
  - So `route_for_dial` falls to `unrouted()` and passes the logical identifier through (`crates/ocx_index/src/lib.rs:306-309,379-391`), even for an `ocx.sh/…` name the home's own index owns.
  - The pull's dial guard then returns at its not-a-rewrite carve-out before any DNS lookup (`lib.rs:429-430`; `crates/ocx_package_manager/src/tasks/pull.rs:772-777`). No flag skips it.
- Digests, caps, completeness, codesign, extraction guards and atomic finalisation come from the pull path unchanged.
- Storage stays keyed by the logical name (`crates/ocx_package_manager/src/tasks/pull.rs:841-842`). So a later `ocx --offline package install ID@DIGEST`, or a locked project's `ocx --offline exec`, needs no routing root.

**Pins** (A1, narrowing the ratified "import merges"). With no `ID`, import writes nothing to the local index. Each named `ID` (`<pkg>` = every tag the snapshot pins for it, `<pkg>:<tag>` = that tag) is adopted by one rule:

- **The home already has a root for the package.** Only tags the home lacks are added. The home's `repository` pointer is kept, because `merge_root` keeps a usable committed root (`crates/ocx_index/src/local_index.rs:829-830`).
- **No home root, and the snapshot root's `repository` is the package's own name.** A bare root is created: `repository` and the adopted tags, no other package-level field. `merge_root`'s first-sight base would copy every field (`local_index.rs:829-838`).
- **Every adopted tag's dispatch object** must hash to the digest the row pins, or nothing is adopted (65).
- **No home root, and the snapshot root's pointer names another repository.** Nothing is adopted. The pin row reports `withheld_routing`, and the remedy is `ocx --index DIR/index …` offline or `ocx index update <ID>` online.
  - A first-sight root would adopt the bundle's pointer (`local_index.rs:833-837`). A local answer then wins on every later online resolve. So a bundle could re-route a package permanently.

**Dependency routing roots are never adopted.** A tag the home pins to another digest stays (`kept_local`, exit 0).

**Order and garbage collection.**

- Pins are adopted strictly after content.
- Imported content has pull semantics and no GC root, so `ocx clean` removes it unless something installs it or a lock pins it (`website/src/content/docs/reference/command-line/commands.md:73`). Import says so on stderr, and the docs name the bundle as the recovery source.

**Bundle file reads** go through one directory capability on `DIR`. Each component is opened with no-follow, the opened handle is checked to be a regular file, and the read is size-capped.

- A snapshot is copied through this capability into a private temp directory before `LocalIndex` reads it. `IndexStore` reads follow symlinks today (`crates/ocx_index/src/store.rs:384,405,424`), and so does `read_bounded` (`crates/ocx_util/src/fs/bounded_read.rs:83,86`).
- Export writes go through the same capability: snapshot rows are written in a private copy, then copied in by create-and-rename relative to an opened parent handle, so no path is re-walked (CWE-59, CWE-367).
- A crashed first export leaves `oci-layout` but no `index.json`; export resumes it and import refuses it (65).

### 4. Fetch-plan report (rules_ocx contract)

`ocx --format json [package] inspect --fetch-plan -p P` emits report root `fetch_plan`.

- **Per package, keyed `<alg>-<hex>` by its leaf digest, so a key is stable across plans:**
  - manifest, config and layer blobs, each with digest, size, `media_type`, the URL ocx would dial after `[mirrors]` routing, and its `bundle_path` (`blobs/<alg>/<hex>`);
  - `self_env` and `entrypoints`.
  - No per-layer `compression`, `strip_components` or `prefix` in v1: under C ocx assembles, so Starlark never needs them. Native extraction (B) would add them additively.
- **Top level:**
  - `env`, the composed interface environment in application order. Entries use `ocx env`'s `kind` and `separator` vocabulary (`crates/ocx_cli/src/api/data/env.rs:52-61`), with a value given as `segments`: `{type: text}` or `{type: package_root, package, path}`;
  - `bundle_files`: the exact `oci-layout` and `index.json` content that, together with the downloaded blobs, forms a content-only bundle `ocx package import` accepts.
- **Paths.** A segment `path` is a parsed `RelativePath`; `..` and absolute paths are refused (65 `fetch_plan_path_escapes`).
- **Credentials.** No credential, token or URL userinfo appears in the plan.

**The narrowed SSRF guarantee.** Every package is routed through `Index::route_for_dial`, the same check a real dial runs (`crates/ocx_index/src/lib.rs:306-311`). The plan therefore never names a host ocx itself would refuse.

- It guards rewritten targets only, so directly named private registries, localhost and `[mirrors]` entries pass, as they do for ocx.
- It cannot protect Bazel from DNS rebinding between plan and fetch, or from redirects the registry issues.
- `http://` appears only for a registry in `OCX_INSECURE_REGISTRIES`, which rules_ocx forwards (`rules_ocx/ocx/private/repo_utils.bzl:51`). The docs name both gaps.

**Contract.** The plan is registered as an additional `OutputMode` on both inspect leaves (`crates/ocx_cli/src/command/contract.rs:30-82`) with a schema golden. It is additive at introduction, and the rules_ocx coupling makes it a one-way door (Metadata).

### 5. Signatures (open question 7)

**Export** carries referrers and cosign sidecar tags by default.

**Import** runs the existing auto-verify hook (`crates/ocx_package_manager/src/tasks/auto_verify.rs:119`) against the layout client.

- Trust anchors come from the six-rung ladder, never from the bundle.
- `trust/trusted_root.json` is advisory. An operator installs it deliberately, through `OCX_SIGSTORE_TRUSTED_ROOT` or a ladder rung.
- A policy-covered subject with no carried signature is found in preflight, before any write, and exits 79 with the new slug `bundle_signatures_missing` (wrapping `no_signatures_found`, `crates/ocx_sign/src/verify/error.rs:36-39`). The message names the remedy, re-export with `--referrers`.
- A carried signature that fails verification refuses its leaf. Leaves already imported stay, each verified, and no pin is adopted.

**Local referrer listing.** Imported referrers are persisted as a subject-keyed listing in the blob tier and read local-first by verification. This ships in the same release as export and import. Without it, auto-verify still asks a registry offline (`auto_verify.rs:35`), and a policy-covered package cannot be installed from an imported home.

### 6. Bazel model and rules_ocx modes (open question 8)

**Default materialiser C.** A spoke repository rule runs, in order:

1. `ocx --format json package inspect --fetch-plan -p P ID@DIGEST`. Verification runs here, against the user's trust ladder.
2. `rctx.download` of each blob to its `bundle_path`, with `sha256` and `canonical_id`.
3. Write each `bundle_files` entry verbatim.
4. `ocx --offline package import --no-verify <bundle>` into a repository-local `OCX_HOME`.
   - The plan already verified these digests, and import re-checks every byte against them.
5. `ocx --offline --format json package which ID@DIGEST`, for the content path.
6. Render launchers from the plan's segments, relative to the runfiles tree, with no `OCX_HOME` and no absolute store path. Launchers unset any inherited `OCX_BINARY_PIN` and `__OCX_ENV_STATE`.

The owner's "Bazel downloader fetches into the output base" holds: every byte comes through Bazel's downloader.

| Mode | Fate |
|---|---|
| Shared `$OCX_HOME` eager (default today) | Kept as the explicit fallback `ocx_home` mode. The default flips in the rules_ocx release that passes the Phase 3 gate. |
| `isolated_home` | Deleted. Its repository-local home survives as step 4. Its own registry fetch and its loss of the trust-root rung (`rules_ocx/AGENTS.md:206-210`) do not survive: Bazel fetches, and the plan verifies against the user's ladder. |
| `bins` (lazy launchers re-entering `ocx exec --pinned`) | Deprecated, and allowed only with `ocx_home`. Hub and spoke repositories give per-tool laziness through Bazel's lazy repository fetch. |

`make_ocx_env()` keeps its explicit `OCX_NO_VERIFY` class for step 1 (`rules_ocx/ocx/private/repo_utils.bzl:72-76`). The Windows `.bat` `%*` launcher (`repo_utils.bzl:1536`) is not carried forward; Phase 3 picks a native executable launcher.

### 7. The eight open questions

| # | Question | Decision |
|---|---|---|
| 1 | Whole-toolchain export | Not a flag on `package export`. Reading `ocx.lock` breaks tier purity (driver 4). Toolchains export by digest identifiers from `ocx --format json status`. |
| 2 | Referrer convention | oras-go: untagged descriptors found by `subject`. No referrers-API fallback-tag index. Cosign sidecar tags are carried as tagged descriptors, because the simplesigning reader discovers them by tag. |
| 3 | Tags | Full logical references in `ref.name`. Tag pins come from the snapshot only. |
| 4 | Absent platforms | The snapshot keeps the full dispatch object, and only listed leaves are present. An offline install of an absent platform exits 81 for that platform only. |
| 5 | Fetch-plan form | The `fetch_plan` report on both inspect commands, digest-only on the OCI tier. |
| 6 | Bazel auth | `--credential_helper` with exact-host patterns, never `*.domain`. S1 measures the minimum Bazel version that drops `Authorization` across origins. rules_ocx pins that minimum, and an HTTPS→HTTP redirect is a hard fail. |
| 7 | Offline signature verify | Decision 5. |
| 8 | rules_ocx modes | Decision 6. |

### 8. Exit codes

No new `ExitCode`. The full slug table is in system design § 6.

- **64.** Usage:
  - `-o` names a non-empty directory with no `oci-layout`;
  - a digest identifier is given without `-p`;
  - an append would pin one tag to two digests (`bundle_pin_conflict`). The next action is a new `DIR` or a digest identifier, not fixing the data;
  - a tag is passed to the OCI-tier `--fetch-plan`.
- **65.** Malformed, incomplete or tampered bundle; unknown bundle version; an invalid manifest (`crates/ocx_oci/src/client/error.rs:55-57`); an invalid plan path.
- **74.** I/O.
- **78.** SSRF refusal.
- **79.**
  - `DIR` is missing;
  - a named `ID` is not in the bundle;
  - a policy-covered subject has no signature (`bundle_signatures_missing`).
- **81.** Export under `--offline` (reusing `offline_mode`, `crates/ocx_package_manager/src/error.rs:54-61`), and an offline install of an absent platform.

## Changed or unhonoured owner decisions

1. **"Carrying [referrers] is a later option" is changed to "carried by default, verified at import, persisted locally, same release".**
   - **Evidence.** Auto-verify asks a registry client even offline and fails closed (`auto_verify.rs:35`). Without carried referrers, a policy-covered package cannot be installed in a registry-less air gap except through `OCX_NO_VERIFY`.
   - **Smallest change.** Reuse `copy_referrers`, the sidecar copy and `options::Referrers`, with `--no-referrers` as the opt-out.
2. **"Import merges a bundle into the home's stores and local index" is narrowed to "content always; tag pins only for named IDs, only tags the home lacks, never a routing pointer and never dependency roots".**
   - **Evidence.** A first-sight merge adopts the bundle's `repository` pointer and routing-only roots (`crates/ocx_index/src/local_index.rs:825-826,833-837`), and a local answer wins on every later resolve (`.claude/rules/subsystem-oci.md:681-682`).
     - A bundle imported "for cmake" could therefore re-route `ocx.sh/python` to any registry, or pin `ocx.sh/python:3.13` to any digest.
     - Simplesigning verification binds the subject digest only (`crates/ocx_sign/src/verify/simplesigning_read.rs:518-530`), so it cannot catch a substituted pin.
     - `--index DIR/index` is not an analogy: it applies to one invocation, and import is permanent.
   - **Alternatives the owner may pick instead:**
     - (a) adopt every tag the home lacks, with no `ID` needed, still never moving an existing pin. This is the original reading, minus pin moves.
     - (b) adopt no pins at all, and use `ocx --index DIR/index` or `OCX_INDEX` for offline tag resolution.
3. **"Layer extraction in Starlark" (Bazel store model) is changed to "Bazel downloads every blob; ocx extracts and assembles in the repository rule" (materialiser C).**
   - **Evidence.** Native extraction bypasses ocx's archive guards (`crates/ocx_util/src/archive/tar.rs:223-305`) and makes Starlark own assembly rules (invariant 1).
   - **What still holds.** The ratified downloader, output base, repository cache, `--downloader_config` and fetch plan all hold.
   - **Native extraction (B)** stays possible as a gated optimisation (§ Bazel materialiser). The owner may require B as the default instead. That makes S2a's hostile half a release gate, and Starlark then owns overlap and codesign rules.
4. **Recommended answer 1 (a lock-reading export flag) is declined.** It was a recommendation, not a ratified decision (driver 4).

## Consequences

**Positive**

- An air gap needs no registry, and integrity is end to end.
- Bazel action keys stop depending on `OCX_HOME` or the output base.
- A bundle seeds an internal registry through `oras cp --from-oci-layout`.
- `LayoutTransport` gives pull, copy and verify a local source and sink.
- Codesign parity holds under Bazel, because ocx extracts.

**Negative**

- New code in seven crates, a new `RootScope` variant and a new GC edge for referrer markers.
- Air-gapped tag resolution for an indirected name needs `--index DIR/index`. A first-sight root with a foreign pointer is withheld.
- Imported content needs an install or a lock to survive `ocx clean`.
- Plan generation reads manifests through ocx, so a no-network rebuild in a fresh output base needs ocx's manifest cache, or `bazel vendor`.
- C stores both the compressed blobs and the extracted tree in the repository directory. rules_ocx may delete the bundle directory after import.

**Risks** are in system design § 12.

## Implementation plan

The DAG and work packages are in system design § 10.

- **Phase 0, spikes.**
  - **S1** (auth and redirects, minimum Bazel version) gates Phase 2.
  - **S2b** (`bazel vendor` and a moved output base with a repository-local home) gates Phase 3.
  - **S3** (`skopeo copy oci:DIR:<ref>`) runs inside WP-7, the acceptance package. If it fails, skopeo leaves driver 7. Bare-tag `ref.name` duplicates are not added, because bare tags collide across packages.
  - **S2a** (benign and hostile native extraction) runs only if B is ever pursued.
- **Phase 1, one release.**
  - Directory capability and `LayoutTransport` (`ocx_util`, `ocx_oci`).
  - `adopt_pins` (`ocx_index`).
  - Export and import tasks (`ocx_package_manager`).
  - Referrer markers and their GC edge (`ocx_store`), and the local-first referrer read (`ocx_sign`).
  - Commands and reports (`ocx_cli`).
  - Interface steps:
    - `cli.json` and `reference/command-line/**` regenerated;
    - report goldens;
    - `CONTRACT` rows;
    - smoke and `command` markers;
    - a `test/SUITE_FLOOR` bump.
  - No `CHANGELOG.md` edit and no migration prose.
- **Phase 2, fetch plan.** Segment rendering (`ocx_package`), `Client::blob_url` (`ocx_oci`), the task and the flag.
- **Phase 3, rules_ocx** (its own ADR). Materialiser C is the default, `isolated_home` is deleted, `bins` is deprecated, and launchers are native on Windows.

**Docs that change:**

- `guides/work-offline.mdx` gains:
  - a bundle section;
  - the air-gap prerequisite, a pinned `OCX_SIGSTORE_TRUSTED_ROOT`;
  - the `clean` caveat;
  - "written only by ocx".
- `explanation/local-index.md`: import is a named, adds-only pin writer.
- `reference/command-line/**` is regenerated.
- In `.claude/rules/`: `product-context.md` #9, `subsystem-oci.md`, `subsystem-cli-commands.md`, `subsystem-package-manager.md`, `subsystem-file-structure.md`, and an `arch-principles.md` ADR row.
- In rules_ocx: `docs/concepts/how-it-works.md`, `docs/guides/offline-mirror.md` and the `AGENTS.md` contract.

## Validation

Each dossier item and each round-1 Block finding maps to a named test in system design § 11:

- the signed round trip;
- the locked-project `ocx --offline exec`;
- routing withheld, and dependency roots never adopted;
- a crash at each export write boundary;
- `oras cp -r` plus `oras discover --oci-layout`;
- the description carried;
- the plan accepted by import;
- a Bazel no-network build, a vendor round trip, a moved output base, and action-key identity across two `OCX_HOME`s.

**Gates:**

- Phase 2 waits for S1.
- Phase 3 waits for S2b and for its Bazel checks on Linux, macOS and Windows.

## Out of scope

- Distro renderers (apt, Chocolatey, Scoop, winget, nfpm).
- New signing, and bundle signing (the extension point is reserved in Decision 2).
- Root `ocx export` and a lock-reading export.
- Home alignment.
- Remote execution for ocx's own build.
- Patch companions and managed-config snapshots.
- An ocx-written `oci-archive`, and reading `.tar` bundles on import (untar first; add when asked).
- `import --dry-run`.

## Links

- `.agents/discussions/bazel-hermetic-package-bundles.md`
- ADRs:
  - `adr_three_tier_cas_storage.md`
  - `adr_index_indirection.md`
  - `adr_servable_index_snapshot.md`
  - `adr_offline_verify_trust_cache.md`
  - `adr_trust_policy.md`
  - `adr_platform_model_unification.md`
  - `adr_cli_high_low_layering.md`
  - `adr_ocx_interface_contract.md`
  - `adr_exit_code_taxonomy.md`
  - `adr_oci_registry_mirror.md`
  - `adr_exec_env_model.md`
  - `adr_bazel_build_adoption.md`
- [ocx-sh/ocx#23](https://github.com/ocx-sh/ocx/issues/23), [bazelbuild/bazel#29660](https://github.com/bazelbuild/bazel/issues/29660)

## Changelog

| Date | Change |
|---|---|
| 2026-11-08 | Proposed. |
| 2026-11-08 | Round 1:<br>- import pins narrowed (A1);<br>- Bazel materialiser inverted to C (A2);<br>- membership by `index.json`;<br>- plan guard = `route_for_dial`;<br>- referrers in the same release;<br>- two one-way doors. |
| 2026-11-08 | Re-validation:<br>- created root is bare;<br>- signature presence in preflight;<br>- handle-relative bundle writes;<br>- resumable first export;<br>- `unrouted` routing corrected. |
