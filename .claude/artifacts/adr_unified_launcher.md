# ADR: One launcher program and one record file on every system, the short vendored layout, atomic launcher publish and refresh, and a runfiles manifest lookup

## Metadata

**Status:** Accepted (accepted under goal-loop authority, 2026-11-26; the owner may revisit)
**Date:** 2026-11-26
**Deciders:** architect worker of a `/hex-architect` run (tier high), inside the unattended goal loop `.agents/goals/bazel-rbe-rules-ocx.md`; orchestrator rulings R-U1…R-U10 (goal-loop authority); the owner rules on the result
**Beads Issue:** N/A
**Related PRD:** `.agents/goals/bazel-rbe-rules-ocx.md` § Context "Second run", ocx items 1-8; [ocx-sh/ocx#785](https://github.com/ocx-sh/ocx/issues/785) (vendored layout), [ocx-sh/ocx#679](https://github.com/ocx-sh/ocx/issues/679) (launcher refresh); rules_ocx `.agents/adr/adr_0003_package_locking.md:497-514` (MR-1…MR-14)
**Tech Strategy Alignment:**
- [x] Rust 2024, Tokio, `serde`/`serde_json` already in the launcher; no new crate, language or runtime
- [x] Deviations: none
**Domain Tags:** storage | api | integration | security | devops
**Supersedes:** the binding sentences listed in § 14 "Amendments" of seven records
**Superseded By:** —
**Companion:** [`system_design_vendor_rules_ocx.md`](./system_design_vendor_rules_ocx.md) (vendored half: § 4.0 layout, § 4.1 manifest, § 4.2 record, C-012 lookup, C-025…C-032 consumer contracts)
**Blast radius:** cross-area. It touches the launcher binary (`crates/ocx_shim`), the record type (`crates/ocx_env_apply`), the store layout (`crates/ocx_store`), the launcher writers, the composer, the toolchain renderer and `ocx clean` (`crates/ocx_package_manager`), trampoline detection (`crates/ocx_config`), the update hand-off (`crates/ocx_setup`), the local index reader (`crates/ocx_index`), the vendor manifest (`crates/ocx_package`) and rules_ocx.
**Reversibility:** one-way door **Medium** for the record `NAME.launch` once it is in installed homes: a home keeps every launcher an ocx wrote until `ocx clean`, and the launcher program reads only the record format of its own release. It stays Medium, not High, because generation directories contain it: a record format change is a new generation, written into a new directory on first use, and the old directory keeps its own launcher bytes beside its own records, so no installed file is ever read by a program of another format. The vendored layout and `vendor-manifest` v2 are two-way until the first release that writes them (no baseline and no ledger entry exist yet, § 3.5). The frozen command lines (`launcher exec`, `launcher shim`, `--project … exec`, `--global exec`) were one-way High before this record and stay so.

## Context

Today each kind of launcher has its own form (research file 1, § 2): Unix scripts for package entry points, deferred tools and toolchain trampolines (`crates/ocx_package_manager/src/launcher/body.rs:15`, `:29`, `:74`); on Windows a native `.exe` hardlinked from one blob plus a one-line sidecar `.shim`, `.shimref` or `.exec` (`body.rs:103-186`, `crates/ocx_shim/src/core.rs:117-121`); and for vendored trees a JSON `.direct` sidecar (`crates/ocx_env_apply/src/direct.rs:23-41`). The Unix launcher program reads `.direct` only (`crates/ocx_shim/src/main.rs:712-714`).

Four writers publish differently (research file 1, § 1). Packages, deferred tools and vendored trees publish one directory with one rename. The toolchain renderer edits `shells/default/bin` entry by entry (`crates/ocx_package_manager/src/tasks/render_toolchain.rs:515-644`); on Windows `hardlink::update` is remove then link (`crates/ocx_store/src/hardlink.rs:91-99`). Nothing refreshes a launcher after an `ocx` upgrade, and the render stamp records what is on disk, never what the running `ocx` would write (`crates/ocx_store/src/file_structure/state_store.rs:116-136`, `crates/ocx_package_manager/src/activation.rs:772-821`).

Under Bazel on Windows, `--enable_runfiles` is off by default and only a runfiles manifest exists; the launcher reads no manifest and exits 78 (`crates/ocx_shim/src/direct.rs:313-388`).

The owner fixed the target (goal file § Rules, § Context "Second run"): the short vendored layout with full-digest clash refusal and one type `VendorLayout` owning the names; one native launcher on Linux, macOS and Windows, in vendored trees and in homes, replacing the Unix scripts; one JSON record `NAME.launch` with an inner `kind`, replacing `.direct`, `.shim`, `.shimref` and `.exec` for every newly written launcher, with no reader for the old kinds in the new launcher; frozen command lines; atomic construction; issue 679 refresh by generation directories; the runfiles tree lookup kept and a manifest lookup added.

## Decision drivers

1. **The owner's rules** (binding, goal file § Rules): atomic by construction; nothing writes into a launcher directory an older `ocx` published; one program and one record everywhere; `manifest.json` is the only interface to rules_ocx and rules_ocx derives no file name; no second construction method; a lock holds platform manifest digests only.
2. **Existing installations keep working**, Windows first ("we need to be very sure to not break existing windows installations").
3. **An older `ocx` sharing the home keeps working on what it published, and every launcher on disk works under either version.** An older prompt in `bin` mode omits a project's toolchain entry while `active` names a newer render directory, until the older `ocx` renders again (§ 10). Nothing either version writes breaks the other.
4. **No hand-rolled wire parser** and the launcher within its 640 KiB per-target budget (`crates/ocx_store/src/shim.rs:19`).
5. **Security parity**: the launcher never starts a program found by a search of its own directory or the current directory; it is not diverted by a benign inherited environment; a hostile clone cannot reach `PATH` through a planted launcher.
6. **Platform claims are claims until CI proves them** (this host is Linux x86_64).

## Industry context and research

Research artifacts: `.agents/research/research_launcher-atomic-publish-and-runfiles.md` (research file 1, measured on this host) and `.agents/research/research_launcher-platform-primitives.md` (research file 2, documentation and source).

- **Generation directories behind one switch** (Nix profiles): build the new generation complete, then one `rename` over a link. Only this design rules out a mixed state by construction (research file 1, § 4). rustup, mise, Volta, Scoop and npm `cmd-shim` refresh file by file.
- **One binary plus a data file named after it** (Scoop `shim.exe` + `NAME.shim`; proto's shim plus `registry.json`) is the shape ocx already uses on Windows.
- **Linux `rename` over a symlink** replaces it with no gap; **macOS** caches a code signature per file, so a signed binary is replaced by a new file and a copy keeps its embedded signature (research file 2, § 4).
- **Windows**: no exchange of two directories; `MoveFileEx` refuses a directory destination; `FSCTL_SET_REPARSE_POINT` overwrites a junction's data in place when the tag matches; atomicity for a concurrent lookup and the privilege need are not established (research file 2, § 2). A running `.exe` can be renamed, not deleted (§ 5).
- **Bazel runfiles manifest**: `<exe>.runfiles_manifest` beside the executable, `MANIFEST` inside `<exe>.runfiles/`; lines `<source> <target>`, escaped since Bazel 7.4.0; an empty target means no file; a remote action never has a usable manifest (research file 2, § 1).
- **Trampoline recognition**: a sibling data file is the cheapest signal that survives a copy (research file 2, § 3).

**Key insight:** if a directory is published once under a name nothing reuses, and the stamp records which name holds which content, then "refresh" is "publish a new directory and switch", and no file is ever rewritten.

## Orchestrator rulings (goal-loop authority)

| ID | Ruling | Reason |
|---|---|---|
| R-U1 | node 26.13.1: keep the refusal; the message names the package and says its `binaries` claim lists a program the content lacks. Owner-revisitable: an explicit opt-out flag naming the tool | a `tools` row whose target does not exist is a false statement in a published interface, and `--manifest-only` cannot tell; the data fix belongs to the publisher of `ocx.sh/nodejs/node` |
| R-U2 | Superseded launcher blobs under `.bin/ocx-shim/` are not collected in this series | bounded, accepted litter (`shim_bin_store.rs:74`) |
| R-U3 | The cross-model pass ran under codex through nox-review from a staged scratch copy; its five findings are B4, B6, W1, W2, W3 of the round-1 fix list | record of the review |
| R-U4 | `vendor-manifest` becomes `schema_version: 2` | the dev build rules_ocx pins wrote v1 with the old layout; a bump lets rules_ocx refuse a stale dev build by number; no baseline and no ledger entry exist, so the gate has nothing to compare |
| R-U5 | `$RUNFILES_MANIFEST_FILE` stays a third root-discovery source, accepted only when it lists the started path as a target; record discovery through a manifest stays deleted | the record always sits beside the started path (MR-3, MR-4); the trust anchor is byte equality between that record and `<root>/<self>.launch`, which no manifest can supply |
| R-U6 | Optional `packages[].index_digest`: the image index digest a root was selected from; absent when the identifier named a platform manifest or the package was reached by digest; never enters a lock | MR-8; per package because a project vendor has several roots; a lock holds platform manifest digests only (owner) |
| R-U7 | Optional top-level `index_lookups[] {path, found}`: every path under the `--index` directory resolution read, relative to it, `/`-separated, sorted; present only with `--index` | MR-9; a list of inputs read, useful to any consumer; no Bazel term |
| R-U8 | MR-2, MR-3, MR-4, MR-10, MR-11 are contracts (system design C-025…C-028; MR-6, MR-12, MR-8 and MR-9 are C-029…C-032) | the consumer depends on them |
| R-U9 | MR-6 refused as written: a bare platform manifest is vendored for the requested `-p P` with `platform = P` and `available_platforms = ["any"]`; no new exit code | a bare manifest records no platform (`resolve.rs:484-516` takes the `Manifest::Image` arm with `Platform::any()`), so ocx cannot decide "serves P"; rules_ocx keeps the platform check, at resolve time through the image index (`index_digest`, `available_platforms`), and vendors the digest it recorded; its lock holds platform manifest digests only |
| R-U10 | MR-12 defect: a vendor run writes nothing into the `--index` directory; `place_package` and `verify_selection` use `manager.read_only_view()` | today both use the writable manager (`tasks/vendor.rs:1011-1017`, `:379`), so a digest-addressed resolve under `LocalWritePolicy::Full` can write there |

## 1. Decisions per open item

| # | Item | Decision |
|---|---|---|
| 1 | Record schema | `peek_kind`, then one plain struct per kind, each with `deny_unknown_fields` and its own `kind` and `schema_version` fields (M1 D). Unknown field, unknown kind, missing or duplicate `kind` → refused. Bound 1 MiB. `@VENDOR_DIR@` only in `direct`. Record name = launcher file name minus one `.exe` on Windows, plus `.launch`. Empty `OCX_BINARY_PIN` = unset everywhere (§ 3) |
| 2 | Vendor manifest | `tools[]` and `packages[].entrypoints[]` gain `record_path`, present exactly when `launcher_path` is; short layout; `index_digest`, `index_lookups`; `schema_version: 2` (R-U4, § 3.5) |
| 3 | Generation key | `LauncherGeneration` over the host blob digest, the record schema version and `HOME_LAUNCHER_REVISION`, exact bytes in § 4.1. Packages: `<package dir>/launchers/<G>/`. Deferred tools: a new home root `deferred/<G>/…`. Toolchain: `shells/default-<16 random hex>/`, its content fingerprint in the stamp (M7 D) |
| 4 | Mixed-version homes | The new `ocx` never writes, renames or deletes `entrypoints/`, `shims/…`, `shells/default` or the v1 stamp; its stamp is a new file `render_stamp.v2.json`; it reads both stamps and the old trampoline signals (§ 10) |
| 5 | Toolchain render | New uniquely named directory per new content, then `active` switched; Unix by `rename` of a staged link; Windows by in-place junction retarget, gated on CI job `junction-proof`; if that proof is red, remove-then-rename ships as the stated exception. The render deletes nothing; `ocx clean` retires (§ 6.4, § 12) |
| 6 | Trampoline detection | Also true for a file whose sibling `NAME.launch` peeks as `toolchain`; the `.exec` and line-two-marker signals stay; a launcher depth guard is added (§ 11) |
| 7 | Refresh points | First use in `compose_roots` and `prepare_lazy`; every render point compares the desired fingerprint with the stamp; new setup phase 1.6 renders the global home (§ 8) |
| 8 | Garbage collection | `ocx clean` alone retires superseded generations and render directories, reported as kind `launchers`; staging is a locked `temp/` scratch (§ 12) |
| 9 | Platform details | Every home launcher is a byte copy of the embedded host blob (M12 K); `ShimBinStore` has no new writer; no Unix script on Windows; macOS signature proven in CI (§ 13) |
| 10 | Runfiles manifest | A fourth root stage, kind `direct` only: `<P>.runfiles/MANIFEST`, `<P>.runfiles_manifest`, then `$RUNFILES_MANIFEST_FILE` only when it lists `P` (R-U5); the root comes from the line whose key ends `/<self>.launch` (§ 5) |
| 11 | Existing packages and installs | No metadata or OCI change; old launcher directories stay and keep calling the frozen command lines (§ 10) |
| — | node 26.13.1 | Refused, with a clearer message (R-U1, M11, § 15) |

## 2. Trade-off matrices

Weights: 5 = binding requirement, 3 = strong driver, 1 = nice to have. Score 1-5; total = Σ weight × score. A tie goes to the option with less new code.

### M1 — record encoding (item 1)

| Criterion (weight) | A. Internally tagged serde enum | B. One flat struct, all fields optional | C. Adjacently tagged `{kind, body}` | D. `peek_kind`, then one plain struct per kind |
|---|---|---|---|---|
| A field of another kind is refused (5) | 5 | 1 | 5 | 5 |
| One decoder module, no hand parser (5) | 5 | 5 | 5 | 5 |
| Inner `kind` key on a flat object (5) | 5 | 5 | 3 | 5 |
| Launcher size (3) | 4 (serde buffers into `Content`) | 5 | 4 | 5 |
| Readable by a human (1) | 5 | 4 | 3 | 5 |
| **Total** | 92 | 74 | 80 | **95** |

**Chosen: D.** `peek_kind` reads `{"kind": …}` with serde and ignores the rest; `decode` then deserialises the whole bytes into the struct of that kind, which carries `kind` as a field and `deny_unknown_fields`. Nothing depends on how serde's tagged-enum path treats unknown keys, and no `Content` buffer reaches the launcher. `EnvRecord` keeps `deny_unknown_fields` (`direct.rs:44-53`). V-L01 proves it per kind.

### M2 — empty `OCX_BINARY_PIN` (item 1)

| Criterion (weight) | A. Empty = unset everywhere | B. Empty = a pin everywhere | C. Keep today's divergence |
|---|---|---|---|
| One meaning across every launcher (5) | 5 | 5 | 1 |
| Matches the launchers most homes hold (`${OCX_BINARY_PIN:-…}`, `body.rs:20`) (3) | 5 | 1 | 3 |
| No ocx producer depends on the other meaning (`binary_pin_value` never yields an empty path, `launcher/generate.rs:112-118`) (3) | 5 | 3 | 3 |
| Loud failure on a broken pin (1) | 2 | 5 | 3 |
| **Total** | **57** | 42 | 26 |

**Chosen: A.** The rule "defined but empty is a pin" came from `cmd.exe`'s `IF DEFINED` (`core.rs:369-374`); no `.cmd` launcher exists any more.

### M3 — manifest version step (item 2, R-U4)

| Criterion (weight) | A. Change v1 in place | B. Bump to `schema_version: 2` |
|---|---|---|
| A consumer pinned to a stale dev build refuses it by number (5) | 1 | 5 |
| No released consumer breaks (5) | 5 | 5 |
| The compat gate stays honest: no baseline, no ledger entry, nothing to compare (3) | 5 | 5 |
| Change size (1) | 5 | 4 |
| **Total** | 50 | **69** |

**Chosen: B.** `crates/ocx_schema/contract/baseline/` holds no `vendor-manifest.json` and `contract/ledger.toml:8` names no `vendor-manifest` document, so the bump needs no ledger entry: the gate reads every field as an addition. rules_ocx reads exactly 2 and refuses 1 (MR-1).

### M4 — where a package's launchers live (item 3)

| Criterion (weight) | a. `<package dir>/launchers/<G>/` | b. `$OCX_HOME/launchers/<G>/packages/<registry>/<shard>/` |
|---|---|---|
| Never writes into a launcher directory an older ocx published (5) | 5 | 5 |
| An older `ocx clean` does the right thing or nothing (5) | 5 (removes it with its package) | 5 (never walks it) |
| No litter under an older ocx (1) | 5 | 1 |
| Lives and dies with its package (3) | 5 | 2 |
| Windows path length (3) | 4 | 4 |
| New GC code (1) | 4 | 2 |
| **Total** | **86** | 71 |

**Chosen: a.** A deliberate exception, stated: `launchers/` is a new child of a package directory an older ocx may have published. The package directory is not a launcher directory, nothing inside `entrypoints/` changes, an older ocx reads only `content`, `refs` and `entrypoints` there, and its `ocx clean` removes the whole package directory, new child included.

### M5 — where a deferred tool's launchers live (item 3)

| Criterion (weight) | a. Inside the existing `shims/…/<shard>/` | b. `shims/<G>/…` in the old root | c. New root `deferred/<G>/…` |
|---|---|---|---|
| Never writes into a launcher directory an older ocx published (5) | 1 | 5 | 5 |
| An older `ocx clean` does the right thing or nothing (`shim_store.rs:83-102` walks unbounded) (5) | 2 | 3 (safe only while no child is named `bin`) | 5 (never walks it) |
| No litter under an older ocx (1) | 5 | 1 | 1 |
| GC rooted in lock pins as today (3) | 5 | 4 | 4 |
| A superseded generation is one directory removal (1) | 2 | 5 | 5 |
| **Total** | 37 | 58 | **68** |

**Chosen: c.** `ShimStore` is parameterised by root and completeness marker (`ShimStore::new(root, marker)`): `fs.shims` stays `shims/` with `bin`, `fs.deferred(&G)` is `deferred/<G>` with `l`. No second store type.

### M6 — switching `active` on Windows (item 5)

| Criterion (weight) | A. In-place junction retarget, the one path | B'. Today's remove-then-rename (`symlink.rs:220-288`) | D. A new `PATH` entry per render |
|---|---|---|---|
| No moment without `active` (5) | 5 (once proven) | 1 | 5 |
| Proven in CI (5) | 3 (until `junction-proof` is green) | 5 | 3 |
| No session-`PATH` rewrite per render (3) | 5 | 5 | 1 |
| Code size (1) | 3 | 5 | 2 |
| **Total** | **58** | 50 | 45 |

**Chosen: A, with no runtime fallback.** If `junction-proof` is red, B' ships instead as the stated exception ("If the junction proof fails, today's remove-then-rename stays and the pull request says so", goal file § Emphasis). Its gap is the bounded window with no `active` (`symlink.rs:220-224`); an interrupted switch is healed by the next render (§ 6.4 step 3) or by the next prompt's refusal plus that render. An `active` that is not a junction (a copied directory, a symlink) is healed by remove-then-create, as C-081 does for any other kind today.

### M7 — naming a toolchain render directory (item 5)

| Criterion (weight) | A. `default-<content fingerprint>` | B. `default-<generation>` | C. `default-<counter>` | D. `default-<16 random hex>`, fingerprint in the stamp |
|---|---|---|---|---|
| Never edits a published directory (5) | 5 | 1 | 5 | 5 |
| A repeated render is a no-op (3) | 5 | 5 | 1 | 5 (stamp fingerprint) |
| A hostile pre-created directory is never reused (3) | 2 (needs a content compare) | 1 | 4 | 5 (exclusive publish under a fresh name) |
| Deterministic in tests (1) | 5 | 5 | 3 | 3 (tests read the stamp) |
| **Total** | 51 | 28 | 43 | **58** |

**Chosen: D.** No compare-before-reuse step and no suffix rule exist.

### M8 — root lookup without a runfiles tree (item 10)

| Criterion (weight) | A. Manifest stage: the key ending `/<self>.launch` gives the root | B. Resolve every value per path through the manifest | C. Keep requiring `--enable_runfiles` |
|---|---|---|---|
| Works on Windows without a runfiles tree (5) | 5 | 5 | 1 |
| No layout name in the launcher (5) | 5 | 5 | 5 |
| One root behind the token, as every other stage (3) | 5 | 1 | 5 |
| Directory-valued `PATH` entries work (3) | 5 | 2 | 5 |
| Launcher size (1) | 4 | 2 | 5 |
| **Total** | **84** | 61 | 65 |

**Chosen: A.**

### M9 — trusting `$RUNFILES_MANIFEST_FILE` (item 10, R-U5)

Threat model: a process whose environment an attacker controls is already lost (`PATH`, `LD_PRELOAD`). The rule here stops a benign inherited variable — an outer test's manifest — from diverting a launcher. The trust anchor is the byte equality of § 5 step 6, not the manifest.

| Criterion (weight) | A. Only when it lists the started path as a target | B. Always (rules_cc order) | C. Never |
|---|---|---|---|
| A benign inherited variable does not divert the launcher (5) | 5 | 1 | 5 |
| Works for a tool started by a test through the test's runfiles (3) | 5 | 5 | 3 |
| Size (1) | 4 | 5 | 5 |
| **Total** | **44** | 25 | 39 |

**Chosen: A**, the rule `$RUNFILES_DIR` follows today (`direct.rs:255-268`). C loses a case: a tool reached through an outer test's manifest when its own manifest is not staged.

### M10 — recognising a native trampoline on Unix (item 6)

| Criterion (weight) | A. Sibling `NAME.launch` of kind `toolchain` | B. Magic bytes in the binary | C. Inode equal to a store blob | D. `argv[0]` |
|---|---|---|---|---|
| Tells a trampoline from a package or deferred launcher of the same bytes (5) | 5 | 1 | 1 | 1 |
| Survives a copy (3) | 5 | 5 | 1 | 5 |
| Cost per exec (1) | 4 | 3 | 5 | 5 |
| **Total** | **44** | 23 | 13 | 25 |

**Chosen: A** (research file 2, § 3).

### M11 — node 26.13.1, a declared binary the content lacks (R-U1)

| Criterion (weight) | A. Refuse the run (today, `tasks/vendor.rs:977-993`) | B. Drop the tool in a full run | C. Keep the row and launcher, warn |
|---|---|---|---|
| The published manifest states nothing false (5) | 5 | 1 (the manifest-only run still lists it) | 1 (a target that does not exist) |
| Both modes' manifests stay equal (C-004) (5) | 5 | 1 | 5 |
| A wrong directory guess is caught (3) | 5 | 3 | 5 |
| A package with one stale claim stays vendorable (3) | 1 | 5 | 5 |
| Matches the home, where the claimed name renders and fails at run (1) | 2 | 2 | 5 |
| **Total** | **70** | 36 | 65 |

**Chosen: A** (R-U1). The vendorability of a defective package is what the publisher's data fix provides.

### M12 — how a home launcher gets its bytes (item 9)

| Criterion (weight) | H. Hardlink from `ShimBinStore` | K. Copy of the embedded blob |
|---|---|---|
| macOS signature stays valid (5) | 4 (many hardlinks: not established) | 5 (a copy keeps the embedded signature, documented) |
| The kernel's image path names exactly one directory (§ 5, step 2) (5) | 3 (macOS `proc_pidpath` of a multiply-linked file may name another link: not established) | 5 |
| One write to the store changes no launcher (3) | 2 | 5 |
| Project homes on another device (3) | 3 (copy fallback) | 5 |
| Disk use (1) | 5 | 2 |
| **Total** | 55 | **82** |

**Chosen: K.** The bytes come from `ocx_store::shim::blob_for(host)`, embedded in the running `ocx`, so no store file needs an integrity check before use. Windows loses the one-inode, one-scan property of today's hardlinks; a scan per new file is the cost (§ 18).

## 3. The record `NAME.launch`

### 3.1 Type and functions (`crates/ocx_env_apply/src/launch.rs`, replacing `direct.rs`)

```rust
/// The one record schema version; also an input of `LauncherGeneration`.
pub const SCHEMA_VERSION: u32 = 1;
pub const RECORD_EXTENSION: &str = "launch";
pub const MAX_RECORD_BYTES: u64 = 1024 * 1024;
/// The bound `peek_kind` reads under; a toolchain record is a few hundred bytes.
pub const MAX_PEEK_BYTES: u64 = 64 * 1024;
pub const VENDOR_DIR_TOKEN: &str = "@VENDOR_DIR@";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchKind { Direct, Package, Deferred, Toolchain }

/// The decoded record; not a serde type itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRecord {
    Direct(DirectRecord),
    Package(PackageRecord),
    Deferred(DeferredRecord),
    Toolchain(ToolchainRecord),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectRecord {
    pub kind: LaunchKind,             // always Direct; checked against the peek
    pub schema_version: u32,
    pub vendor_id: String,
    #[serde(rename = "self")]
    pub self_path: String,
    pub program: String,
    pub args: Vec<String>,
    pub unset: Vec<String>,
    pub env: Vec<EnvRecord>,          // EnvRecord unchanged, deny_unknown_fields (direct.rs:44-53)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRecord { pub kind: LaunchKind, pub schema_version: u32, pub package_root: String }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredRecord { pub kind: LaunchKind, pub schema_version: u32, pub identifier: String }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainRecord {
    pub kind: LaunchKind,
    pub schema_version: u32,
    pub tier: ToolchainTier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocx: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolchainTier { Project, Global }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    Malformed(String),       // serde_json's message: unknown field, missing or unknown kind, duplicate key, type mismatch
    TooLarge,
    KindMismatch,            // the struct's `kind` differs from the peek
    SchemaVersion(u32),
    InvalidKey(String),
    ReservedKey(String),
    PathNotInRoot(String),
    NulByte(String),
    TokenOutsideDirect(String),
    NotAbsolute(String),
    NotPinned(String),
    TierMismatch,            // project without project_root, or global with one
}
// Display impls lowercase, no period. `std::error::Error` impl arrives with the vendor task's families! rung.

impl LaunchRecord {
    /// At most MAX_RECORD_BYTES; `peek_kind`; `serde_json::from_slice` into that kind's struct;
    /// `kind` equal to the peek; `validate`; the NUL scan over args, values and separators.
    pub fn decode(bytes: &[u8], windows: bool) -> Result<Self, RecordError>;
    /// Compact `serde_json::to_vec` of the kind's struct (`kind` first, fields in declaration order), then `\n`.
    pub fn encode(&self) -> Vec<u8>;
    pub fn validate(&self, windows: bool) -> Result<(), RecordError>;
    pub fn kind(&self) -> LaunchKind;
}

/// `{"kind": LaunchKind}` with every other key ignored; `None` on any error.
/// The one non-validating reader: trampoline detection must recognise a record of a later schema.
pub fn peek_kind(bytes: &[u8]) -> Option<LaunchKind>;

/// The record path for a launcher path: its directory, its file name minus one trailing `.exe`
/// (ASCII case-insensitive) when `windows`, then `.launch`.
pub fn record_path_for(launcher: &str, windows: bool) -> String;
```

`DirectSidecar`, `SIDECAR_EXTENSION` and `SidecarError` are renamed in place (`AGENTS.md` § Stability tiers); no alias stays. Serde's duplicate-field check refuses a second `kind` key; an unknown `kind` value fails `peek_kind`'s enum.

### 3.2 Validation per kind

Every kind: `schema_version == SCHEMA_VERSION`; at most 1 MiB; no NUL in any string.

| Kind | Rule |
|---|---|
| `direct` | the rules of `DirectSidecar::validate` today (`direct.rs:85-132`): key shape, reserved `OCX_BINARY_PIN` and `__OCX_*`, `unset`/`env` overlap, `self` and the part of `program` after `@VENDOR_DIR@/` obey the manifest path rules, the token nowhere else in them |
| `package` | `package_root` absolute on any host (`core.rs:323-333`), no CR or LF, no token |
| `deferred` | `identifier` passes `is_pinned_identifier` (`core.rs:292-319`), no token |
| `toolchain` | `tier = project` ⇔ `project_root` present and absolute; `ocx`, when present, absolute and not a batch file (`is_batch_target`, `direct.rs:535-543`); no CR, LF or token |

Unknown-field policy: refused at every level. A later schema is a new `SCHEMA_VERSION`, which changes the generation key (§ 4.1), so it is written only into a new directory beside its own launcher bytes. `toolchain`'s two optional keys are not the `RenderStamp` trap (`state_store.rs:115`): the tier is explicit, and a dropped `project_root` under `project` is `TierMismatch`.

### 3.3 One full example per kind

`direct` (vendored, `l/cmake.launch`):

```json
{"kind":"direct","schema_version":1,"vendor_id":"sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a","self":"l/cmake","program":"@VENDOR_DIR@/p/1f2e3d4c5b6a7988/c/bin/cmake","args":[],"unset":["OCX_ANNOUNCE_GIT_TOKEN","OCX_BINARY_PIN","OCX_IDENTITY_TOKEN","OCX_KEY_PASSWORD","OCX_SIGNING_KEY","OCX_TELEMETRY_HEADERS","__OCX_ENV_STATE"],"env":[{"key":"PATH","value":"@VENDOR_DIR@/p/1f2e3d4c5b6a7988/c/bin","kind":"path"},{"key":"CMAKE_ROOT","value":"@VENDOR_DIR@/p/1f2e3d4c5b6a7988/c/share/cmake-3.31","kind":"constant"}]}
```

`package` (home entry point, `<package dir>/launchers/3c9e0a1d7b2f4e68/npm.launch`):

```json
{"kind":"package","schema_version":1,"package_root":"/home/u/.ocx/packages/ocx.sh/sha256/5a/0c1e9d2b7f3a4c5a0c1e9d2b7f3a4c"}
```

`deferred` (`$OCX_HOME/deferred/3c9e0a1d7b2f4e68/ocx.sh/kitware/cmake/sha256/1f/2e3d4c5b6a79881f2e3d4c5b6a7988/l/cmake.launch`):

```json
{"kind":"deferred","schema_version":1,"identifier":"ocx.sh/kitware/cmake:3.31.6@sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988"}
```

`toolchain` (project trampoline, `<home>/toolchain/shells/default-8d41c2e07a95b3f6/bin/cmake.launch`):

```json
{"kind":"toolchain","schema_version":1,"tier":"project","project_root":"/work/app","ocx":"/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx"}
```

A global trampoline: `{"kind":"toolchain","schema_version":1,"tier":"global","ocx":"…"}`.

### 3.4 Which program a home launcher starts, and with what

`NAME` is the file name of the image path of § 5 step 2, minus one `.exe` on Windows. A launcher reached through a symlink therefore passes the target's name, not the link's; today's script passed the link's (`${0##*/}`).

The program `ocx` is resolved in this order, every rung absolute, never a search of the launcher's directory or the current directory (CWE-427):

1. `OCX_BINARY_PIN` when non-empty. It must be absolute, else 78.
2. The record's `ocx` (kind `toolchain` only).
3. A `PATH` walk done by the launcher itself: entries in order; skip an empty entry, a relative entry (`is_absolute_path`, `core.rs:323-333`) and the launcher's own directory (ASCII case and separators folded on Windows); the candidate is `<entry>/ocx` on Unix (a regular executable file) or `<entry>\ocx.exe` on Windows (no `PATHEXT`, so never `.bat` or `.cmd`); a candidate with a sibling `ocx.launch` that is a regular file is another launcher and is skipped. None → 78 "no ocx found on PATH".

A program whose name is a batch file (`is_batch_target`) is refused with 78 on every rung. The program is passed absolute: Unix `execve` of the path, never `execvp`; Windows `CreateProcessW` with `lpApplicationName` always set, so `ResolvedProgram::explicit` is always true and the NULL rung (`core.rs:371-384`) is deleted. An entry point named `ocx` therefore starts the real `ocx`: its own directory and every other launcher are skipped.

Package and deferred records bake no `ocx`, so their bytes stay a function of the path they sit in and § 4.1's claim holds (the generation fixes every byte of those directories).

| Kind | Command | Environment change |
|---|---|---|
| `direct` | `program args… user-args…` (system design C-012…C-017) | `unset`, then `env` |
| `package` | `ocx launcher exec <package_root> -- NAME user-args…` | `__OCX_LAUNCHER_DEPTH` + 1; containment check when `OCX_HOME` is set (`core.rs:340-345`), on every OS |
| `deferred` | `ocx launcher shim <identifier> -- NAME user-args…` | `__OCX_LAUNCHER_DEPTH` + 1 |
| `toolchain` | `ocx --project <project_root> exec -- NAME user-args…` or `ocx --global exec -- NAME user-args…` | `__OCX_LAUNCHER_DEPTH` + 1; `OCX_GLOBAL` and `OCX_PROJECT` removed (`main.rs:345-355`) |

**Depth guard.** No exec-depth guard exists today (no match in `crates/ocx_env/src`). `__OCX_LAUNCHER_DEPTH` (registered in `ocx_env`, private, passed to children unchanged by `ocx`) counts home-launcher hops; a home launcher that reads 32 or more exits 78 "launcher loop". It ends a loop no single check sees: two launchers whose resolved programs start each other.

The command tokens are frozen (`core.rs:389`, `:395`, `:402`; `body.rs:15-100`). Unix replaces the process; Windows keeps the job object and the `CommandLineToArgvW` quoter (`core.rs:407-442`). The launcher no longer parses `.shim`, `.shimref`, `.exec` or `.direct`; `SIDECAR_PROBE_ORDER` and the one-line parsers (`core.rs:117-319`) are deleted from it.

### 3.5 Vendor manifest changes

`schema_version` becomes 2 (R-U4). Paths follow the short layout (§ 4.2). `tools[]` and `packages[].entrypoints[]` gain `record_path`. Two optional fields: `packages[].index_digest` (R-U6) and top-level `index_lookups[]` (R-U7). Full schema and example: system design § 4.1.

- **`index_digest`.** `resolve` keeps the selected image index as `chain[0]` with `ChainRole::Index` (`tasks/resolve.rs:474-475`, `:524-529`); `read_closures` drops it today (`tasks/vendor.rs:335-339`). It is kept as `SelectedRoot.index_digest` and written for roots only in `build_manifest` (`tasks/vendor.rs:511-521`). It never enters a lock.
- **`index_lookups`.** Every read of the local index funnels through `read_index_document` (`crates/ocx_index/src/store.rs:624-638`; `NotFound` → `Ok(None)` at `:635`; callers `:157`, `:396`, `:415`, `:432`, `:480`). `IndexStore` gains an optional lookup recorder, set only on the store built for `--index` (`crates/ocx_cli/src/app/context.rs:358-363`, flag at `app/context_options.rs:120-121`) and reached through the manager. Each call records the path relative to the store root and whether it was found; the vendor task sorts and dedupes them.

## 4. Generation key and directory layouts

### 4.1 The key, as exact bytes

```rust
// crates/ocx_store/src/launcher_generation.rs
/// Bumped when a writer change must reach launcher directories already on disk.
pub const HOME_LAUNCHER_REVISION: u32 = 1;

pub struct LauncherGeneration(String); // 16 lowercase hex

impl LauncherGeneration {
    pub fn of(blob: &[u8], schema_version: u32, revision: u32) -> Self;
    /// `of(shim::host_blob()?, ocx_env_apply::launch::SCHEMA_VERSION, HOME_LAUNCHER_REVISION)`.
    pub fn host() -> Option<Self>;
    pub fn as_str(&self) -> &str;
}

/// SHA-256 over the render input below; 64 lowercase hex, stored in the stamp.
pub fn render_fingerprint(generation: &LauncherGeneration, records: &BTreeMap<String, Vec<u8>>) -> String;
```

**`LauncherGeneration::of`** hashes, with SHA-256, the concatenation of these ASCII lines, each ended by one `0x0A`, numbers in decimal with no sign and no leading zeros, hex lowercase:

```
ocx.launcher-generation.v1
blob=<64 hex: SHA-256 of the blob bytes>
schema=<schema_version>
revision=<revision>
```

`G` is the first 16 hex characters of the digest. No field holds a `0x0A`, so the line ends delimit every field.

**`render_fingerprint`** hashes, with SHA-256:

```
ocx.toolchain-render.v1\n
generation=<G>\n
names=<count>\n
then, per name in byte order: <byte length of name>:<name bytes><byte length of record>:<record bytes>\n
```

The lengths are decimal, so a name or a record may hold any byte, the record's own trailing `\n` included. The fingerprint is the full 64 hex.

**Worked example 1** (`of(b"", 1, 1)`; the blob is the empty byte string, whose SHA-256 is `e3b0…b855`). Input, 117 bytes, shown as a Rust byte string:

```
b"ocx.launcher-generation.v1\nblob=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\nschema=1\nrevision=1\n"
```

`G` = `d7c9115eb03568da` (16 hex).

**Worked example 2** (`render_fingerprint(G = "0123456789abcdef", {"cmake" → record})`, the record being the 56 bytes `{"kind":"toolchain","schema_version":1,"tier":"global"}` followed by `\n`). Input, 127 bytes:

```
b"ocx.toolchain-render.v1\ngeneration=0123456789abcdef\nnames=1\n5:cmake56:{\"kind\":\"toolchain\",\"schema_version\":1,\"tier\":\"global\"}\n\n"
```

Fingerprint = `202755de66972decba3905023b7f5f3303c0472810eec3a7f0ffe02691de8f68` (64 hex).

Package and deferred records hold only the package root or the pinned identifier, which the directory path already fixes, and their launcher bytes are the blob. So `G` determines every byte of those directories, and a complete directory under `G` is current. Toolchain records bake the project root and the `ocx` path, so the toolchain directory's content is recorded as its fingerprint in the stamp. The baked `ocx` is `$OCX_HOME/symlinks/…/current/content/bin/ocx` (`launcher/generate.rs:205-214`), stable across `ocx self update`.

A host with no blob (`host_blob()` is `None`; no supported pair, `shim.rs:28-52`) writes no home launcher and fails the composition that needs one with the existing "no launcher blob for this host" error.

### 4.2 Vendored tree (owner-fixed; names owned by `VendorLayout`)

Before (`vendor_layout.rs:54-78`, `tasks/vendor.rs:491`):

```
DIR/manifest.json
DIR/packages/<registry>/<alg>/<2 hex>/<30 hex>/content/…
DIR/packages/<registry>/<alg>/<2 hex>/<30 hex>/entrypoints/NAME[.exe] + NAME.direct
DIR/selections/default/shells/default/bin/NAME[.exe] + NAME.direct
```

After:

```
DIR/manifest.json
DIR/l/NAME[.exe]                      selection launcher
DIR/l/NAME.launch                     its record, kind direct
DIR/p/<HEX16>/c/…                     package content
DIR/p/<HEX16>/e/NAME[.exe]            package entry-point launcher
DIR/p/<HEX16>/e/NAME.launch           its record, kind direct
```

`HEX16` = the first 16 hex characters of the digest's hex part, without the algorithm. `ocx vendor` compares the full digests of every package of the run and refuses two that share a `HEX16` (65 `vendor_path_unrepresentable`, naming both).

```rust
impl VendorLayout {
    /// Names ocx never writes, so a consumer may use them; `check_relative_path` refuses them as a segment.
    pub const RESERVED_SEGMENTS: &'static [&'static str] = &["_ocx_reserved"];
    pub fn manifest_file(&self) -> PathBuf;                                        // DIR/manifest.json
    pub fn relative_launcher_dir() -> &'static str;                                // "l"
    pub fn relative_launcher(name: &str, windows: bool) -> String;                 // l/NAME[.exe]
    pub fn relative_record(name: &str) -> String;                                  // l/NAME.launch
    pub fn package_keys<'a>(digests: impl IntoIterator<Item = &'a Digest>)
        -> Result<BTreeMap<Digest, PackageKey>, PackageKeyClash>;
    pub fn relative_package_dir(key: &PackageKey) -> String;                       // p/HEX16
    pub fn relative_content_dir(key: &PackageKey) -> String;                       // p/HEX16/c
    pub fn relative_entrypoint_dir(key: &PackageKey) -> String;                    // p/HEX16/e
    pub fn relative_entrypoint_launcher(key: &PackageKey, name: &str, windows: bool) -> String;
    pub fn relative_entrypoint_record(key: &PackageKey, name: &str) -> String;
    /// Today's rules (`vendor_layout.rs:91-110`) plus `PathViolation::ReservedSegment`
    /// for a segment equal to a RESERVED_SEGMENTS entry, ASCII case-insensitive.
    pub fn check_relative_path(path: &str, platform_is_windows: bool) -> Result<(), PathViolation>;
    pub fn resolve(&self, relative: &str) -> PathBuf;                              // unchanged
}
pub struct PackageKey(String);             // 16 hex
pub struct PackageKeyClash { pub first: Digest, pub second: Digest }
```

The composer's vendor lane takes its paths from these functions: `ComposePaths::vendor(token_root, keys)` replaces `VENDOR_PACKAGE_SUFFIX_COMPONENTS` and the hand-written tail in `VendorLane::package_dir` (`composer.rs:800-802`, `:946-964`). `install_path_for` returns the content directory and the entry-point directory as two paths, because the vendored names (`c`, `e`) are not the home's (`content`, `launchers/<G>`).

### 4.3 Home

Before:

```
$OCX_HOME/packages/<reg>/<alg>/<2>/<30>/entrypoints/NAME            Unix script
                                       /entrypoints/NAME.exe + NAME.shim + NAME (script)   Windows
$OCX_HOME/shims/<reg>/<repo>/<alg>/<2>/<30>/{digest, refs/blobs/, bin/NAME[.exe] + NAME.shimref}
<home>/toolchain/{.gitignore, active -> shells/default, links/…, shells/default/bin/NAME[.exe] + NAME.exec}
$OCX_HOME/state/projects/<key>/render_stamp.json, $OCX_HOME/state/render_stamp.json   v1
```

After (old entries stay where an older ocx wrote them; the new ocx adds only):

```
$OCX_HOME/packages/<reg>/<alg>/<2>/<30>/launchers/<G>/NAME[.exe] + NAME.launch          kind package
$OCX_HOME/deferred/<G>/<reg>/<repo>/<alg>/<2>/<30>/{digest, refs/blobs/, l/NAME[.exe] + l/NAME.launch}   kind deferred
<home>/toolchain/{.gitignore, active -> shells/default-<16 random hex>, links/…,
                  shells/default-<16 random hex>/bin/NAME[.exe] + NAME.launch}           kind toolchain
$OCX_HOME/state/projects/<key>/render_stamp.v2.json, $OCX_HOME/state/render_stamp.v2.json
```

`ToolchainHome` gains `render_dir(name) -> PathBuf` and `active_target_name() -> Option<String>`; `TREE_OWN_DEPTH1_NAMES` is unchanged (`toolchain_store.rs:33`). `StateStore` gains `render_stamp_v2_file(key)` and `global_render_stamp_v2_file()`.

## 5. The launcher's lookup, as numbered steps

1. **Started path `P`.** Unix: `argv[0]` joined onto the working directory when it holds `/`, else the first `PATH` hit (`direct.rs:287-302`). Windows: `GetModuleFileNameW` (`main.rs:256-280`).
2. **Image path `I`, from the kernel's own record of the loaded image, resolved once.** Linux: `readlink("/proc/self/exe")`; a value ending in ` (deleted)` exits 78. macOS: `proc_pidpath(getpid())`. Windows: `NtQueryVirtualMemory` with `MemoryMappedFilenameInformation` on the module base of `GetModuleHandleW(NULL)` gives the NT path of the mapped file (the call `GetMappedFileNameW` wraps; that wrapper cannot return a name longer than 260 characters, so the launcher calls the query itself); it is opened as `\\?\GLOBALROOT<NT path>` with `FILE_FLAG_BACKUP_SEMANTICS` and resolved with `GetFinalPathNameByHandleW(FILE_NAME_NORMALIZED | VOLUME_NAME_DOS)`, which names the render directory behind the `active` junction (claim, V-L19). No step compares a `stat` with a later `open`.
3. **Home kinds.** Read `record_path_for(I)` with `read_regular_bounded` (`direct.rs:94-142`). A record of kind `package`, `deferred` or `toolchain` is decoded and run per § 3.4, `NAME` from `I`. A launcher started through a switched `active` thus reads the record of the directory the kernel loaded it from, whether the old and new directories hold the same bytes or not.
4. **Kind `direct`** (no record beside `I`, or one of kind `direct`) keeps the started-path rules, because a runfiles tree reaches the launcher through links and a canonical own path fails every Bazel leg (`adr_vendor_rules_ocx_remote_execution.md` § Industry context, spike 6). On Unix `stat(P)` must have the device and inode of the running image, else 78 (V-61 of the system design). The record is read at `record_path_for(P)`; absent, or not kind `direct` → 78 naming the path. This is what keeps rules_ocx's form working: it links the launcher and the record under their manifest basenames into one directory (MR-4), and the launcher finds the record from the name it was started by.
5. **`direct`: root stages 0-2, unchanged** (`direct.rs:350-369`): (0) `P` minus `/<self>`; (1) entries of `<P>.runfiles`; (2) entries of `$RUNFILES_DIR` when `P` lies under it. A candidate `C` is accepted when `<C>/<self>.launch` is byte-identical to the record read in step 4 (`accepts`, `direct.rs:271-281`).
6. **`direct`: stage 3, the runfiles manifest** (no tree). Sources, in order: (i) `<P>.runfiles/MANIFEST`; (ii) `<P>.runfiles_manifest`; (iii) `$RUNFILES_MANIFEST_FILE`, only when it holds a line whose target equals `P` (ASCII case and separators folded on Windows). Each is opened with `read_regular_bounded`'s rule (`O_NONBLOCK`, `fstat` on the handle, regular file only) and read as a stream; a file over 64 MiB exits 78 "runfiles manifest larger than 64 MiB"; a line over 64 KiB or holding NUL exits 78. The bytes are not transcoded on Unix. On Windows the line stays bytes until a candidate root is derived; that root is decoded once as UTF-8 into an `OsString`, because Bazel 8.0 and later write non-ASCII Windows paths as UTF-8, and never through the ANSI code page or UTF-16. A line that is not UTF-8 cannot name a Windows path Bazel wrote and is skipped. This is the one place the fix list's "no transcoding" is narrowed, and why. Parsing follows rules_cc: split at the first space; a line starting with a space is escaped (Bazel 7.4.0 and later): its key unescapes `\s`, `\n`, `\b` and its target `\n`, `\b`; other `\x` stay two characters; an unescaped line keeps backslashes; an empty target is skipped. A line is a candidate when its key ends with `/` + `<self>` + `.launch` (folded on Windows) and its target, with `\` read as `/` on Windows, ends with `/<self>.launch`; the candidate root is the target minus that suffix. Candidates are deduplicated by root (`--legacy_external_runfiles` lists one file twice) before the cap of 4096, in file order; all candidate records read together are capped at 16 MiB. A root must be absolute, UTF-8, free of NUL, CR and LF, have no `.` or `..` segment and no path-list separator; else the candidate is rejected. Each goes through the same `accepts`. `RUNFILES_MANIFEST_ONLY` is never read. Minimum Bazel: 7.4.0 for escaped lines, 8.0 for non-ASCII paths on Windows. The Windows file name `NAME.exe.runfiles_manifest` and its line shape are unverified until V-L46 passes.
7. **No candidate** → 78 naming at most eight rejected candidates and the count (`direct.rs:371-387`); on Windows the hint `--enable_runfiles` stays, because a manifest can be absent too. The launcher exits 78 only when neither a tree nor a manifest gives a root (MR-13).
8. The accepted root is made absolute lexically and refused when it holds the path-list separator (`direct.rs:358-364`).

`LookupFs` (`direct.rs:47-60`) gains `image_path() -> Result<String, ReadError>` and `read_lines(path, max_bytes, max_line) -> Result<Option<Box<dyn Iterator<Item = Result<Vec<u8>, ReadError>>>>, ReadError>`, so every step runs in unit tests without a disk.

**Why `$RUNFILES_MANIFEST_FILE` needs no wider trust** (R-U5): a launcher started by its real path in the external repository is found by stage 0, and a copy in `bazel-out` has its own manifest beside it, sources (i) and (ii). Source (iii) serves a tool reached through an outer test's runfiles whose own manifest is not staged.

## 6. Publish sequences

### 6.1 Helpers

```rust
// crates/ocx_util/src/fs/publish.rs (domain-free)
pub enum Published { New, AlreadyThere }
/// rename(2) of `staged` onto `target`. Unix: an absent or empty target is replaced; ENOTEMPTY and
/// EEXIST map to AlreadyThere. Windows: an absent target only; an existing one, empty or not, is
/// AlreadyThere. Transient retry as `rename_with_windows_retry`. `staged` stays for the caller.
pub async fn publish_dir(staged: &Path, target: &Path) -> std::io::Result<Published>;
/// Points `link` at `target`: Unix stages a sibling link by exclusive create and renames it over
/// `link`; Windows sets the junction's mount-point data in place (FSCTL_SET_REPARSE_POINT on a
/// handle opened with FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS). No fallback at
/// run time (M6). An absent `link` is created; a `link` that is not a junction or symlink is an error.
pub fn retarget_link(target: &Path, link: &Path) -> std::io::Result<()>;
/// `lstat`s `dir` and acts only on a real directory (never a symlink or reparse point); renames it
/// to a sibling `.gc-<random>`, then removes that. A refused rename leaves `dir` whole; ENOTEMPTY and
/// EBUSY during removal are left for the next call; EXDEV (overlayfs) falls back to recursive removal
/// in place. Also sweeps sibling `.gc-*` and `.staging-*` debris it finds.
pub async fn retire_dir(dir: &Path) -> std::io::Result<Retired>;
pub enum Retired { Removed, LeftForLater }

// crates/ocx_store/src/file_structure/temp_store.rs (moved from tasks/export_archive.rs:31-56)
/// A unique `temp/<prefix>-…` directory whose lock file exists before the directory and is held
/// until drop, so `ocx clean` skips it (`temp_store.rs:150-164`).
pub async fn scratch(&self, prefix: &str) -> Result<LockedScratch>;
```

No duplicate: `publish_shim_dir` (`prepare_lazy.rs:237-269`) becomes a caller of `publish_dir`, and `StagedTree::publish` (`vendor_layout.rs:172-216`) keeps its normalisation and calls `publish_dir`, mapping `AlreadyThere` to its existing refusal (a non-empty `DIR` is the user's, never converged on). `retire_dir` is new: nothing today removes a published directory by renaming it first.

**Completeness check.** A launcher directory is complete when it is a real directory (`lstat`, no link or reparse point) and holds, for every expected name, `NAME[.exe]` and `NAME.launch` as regular files. It is applied to an existing target before trusting it and to the winner after `AlreadyThere`.

**Name checks before staging.** The writer computes the whole path set first: two names equal after ASCII case folding on a Windows or macOS target (`fold_case_insensitive`, `tasks/toolchain_names.rs`) collapse as today; a tool or entry-point name ending in `.launch` (ASCII case-insensitive) would collide with a record and is refused (vendor: 65 `vendor_path_unrepresentable`; home writers: that name gets no launcher and the run warns, as C-050 skips today).

### 6.2 Package entry points

On pull (unchanged position, `tasks/pull.rs:696-707`): `launcher::generate` writes `launchers/<G>/` into the locked temp package directory, which is moved into `packages/` whole. **Commit point:** the existing package move.

On first use (`compose_roots`, for every package of the closure whose entry points are admitted on `PATH`):

1. `G = LauncherGeneration::host()`; `D = <package dir>/launchers/<G>`.
2. `D` passes the completeness check → done. `D` exists and fails it → `retire_dir(D)`, warn, continue with step 3; a `LeftForLater` result means `D` could not be moved away, so the composition takes the read-only fallback of step 8.
3. `S = fs.temp.scratch("launchers")` (lock held).
4. In `S`: per name, write the embedded blob to `S/NAME[.exe]` (mode 0755 on Unix), write `S/NAME.launch` (`LaunchRecord::encode`), `sync_data` each.
5. `mkdir(<package dir>/launchers)` without `-p`; `EEXIST` is fine; `ENOENT` means the package directory is gone → the composition fails as for a package that is not installed, and no package directory is recreated.
6. **Commit point:** `publish_dir(S, D)`. `AlreadyThere` → the winner must pass the completeness check; discard `S`.
7. The composer emits `D` as the synthetic `PATH` entry in place of `<package dir>/entrypoints` (`composer.rs:759-767`).
8. **Read-only or foreign-owned home.** When step 4 or 5 fails for permission or a read-only file system: a complete `<package dir>/entrypoints/` written by an older release is composed instead, whole, with one warning; otherwise the composition fails naming the path, exit 77 for a permission refusal and 74 otherwise (the existing I/O classification).

`entrypoints/` is never written again.

### 6.3 Deferred tools

`prepare_lazy` (`prepare_lazy.rs:63-98`) with `fs.deferred(&G).shim_dir(pinned)`: completeness check of `l/` → stage in `fs.temp.scratch("deferred")` (the clean race fix, § 12) → `digest`, `refs/blobs/`, then `l/` last → `prepare_lazy` creates the missing parents `deferred/<G>/<registry>/<repository>/<alg>/<2 hex>` with `create_dir_all` (parents are not publications) → **commit point** `publish_dir`. The composer emits `<tree>/l` (`composer.rs:993-997`).

### 6.4 Toolchain render

Inside the existing `lock_scoped("toolchain-render")`, after `refuse_symlinked_home` and `refuse_symlinked_home_leaves` (`render_toolchain.rs:205-308`), which stay ahead of every step below:

1. Compute the exposed names, each record's bytes, `G` and `F = render_fingerprint(G, records)`.
2. Read the v2 stamp. **No-op:** its `fingerprint` is `F`, `active` names `shells/<stamp.shell_dir>`, and that directory matches the stamp's `bin_fingerprint` → go to step 7.
3. **Re-switch:** the stamp's `fingerprint` is `F` and `shells/<stamp.shell_dir>` matches its `bin_fingerprint`, but `active` names something else (an older ocx healed it to `shells/default`) → `T = stamp.shell_dir`, `T0 = stamp.previous_shell_dir`, go to step 6.
4. Create `S = shells/.staging-<16 random hex>` by exclusive `create_dir`, owner-only; write `S/bin/NAME[.exe]` (the embedded blob, 0755 on Unix) and `S/bin/NAME.launch` for every name.
5. `T = default-<16 random hex>`; `publish_dir(S, shells/T)`; `AlreadyThere` → a new random name, at most eight tries. `T0` = the name `active` named before this render; on a first render, with no earlier `active` target, `T0` is empty and `ocx clean` keeps only `shell_dir`.
6. **Commit point:** `retarget_link(shells/T, active)`.
7. Reconcile `links/` as today (`render_toolchain.rs:774-864`).
8. Write the v2 stamp last: `shell_dir = T`, `previous_shell_dir = T0`, `fingerprint = F`, `generation = G`, the bin fingerprint over `shells/T/bin`.

The render deletes nothing. The stamp names exactly two directories `ocx clean` keeps: `shell_dir` and `previous_shell_dir` (§ 12). A crash before step 6 leaves `active` and its directory as they were. A crash after it leaves a complete new directory behind `active` and a stamp that does not name it; the prompt gate refuses it (§ 8.3), and the next render's step 2 sees no match and publishes again.

### 6.5 Vendored tree

Unchanged: `StagedTree` beside `DIR`, launcher blob then record per name, `manifest.json` last, one rename (`vendor_layout.rs:127-216`, `tasks/vendor.rs:130-154`). A vendored tree is never refreshed. A vendor run writes nothing into the `--index` directory (R-U10).

## 7. Same-generation and cross-generation switches

A switch between two render directories of the same generation changes the records (another lock, another project root) and not the launcher bytes. A switch across generations changes both. In both cases a launcher started through `active` just before the switch reads the record of the directory the kernel loaded it from (§ 5 step 3), so no launcher pairs its bytes with another directory's record. `ocx clean` never retires the previous directory, so that read always finds its file.

## 8. Refresh points and the stale-stamp gap (item 7)

### 8.1 Points

1. **First use.** A new `G` makes `launchers/<G>` and `deferred/<G>/…` absent, so the next composition writes them (§ 6.2, § 6.3).
2. **Every toolchain render point** (`ocx pull`, `add`, `remove`, `lock`, `update`, `upgrade`) compares the desired `F` with the stamp's `fingerprint` (§ 6.4 step 2), not disk with stamp. An upgrade that changed `G` renders a new directory at the next render point.
3. **The update hand-off.** `ocx_setup::run` (`crates/ocx_setup/src/lib.rs:304`) gains phase 1.6 after the select: when `$OCX_HOME/ocx.lock` exists, `render_toolchain` with `RenderStampScope::Global`. It runs with and without `--handoff`, because it heals and never introduces. A render failure warns and never fails setup: phase 1 is the commit point (`adr_self_update_handoff.md:47-50`). The hand-off spawns the new binary's own setup (`tasks/update_check.rs:309-358`), so the new code renders.
4. **Project homes** refresh at their next render point. Until then their trampolines keep working: every older trampoline calls a frozen command line that the new `ocx` serves.

### 8.2 Render stamp v2

```rust
pub struct RenderStampV2 {         // file render_stamp.v2.json, v = 2
    v: u8,
    pub home: PathBuf,
    pub scope: RenderStampScope,
    pub shell_dir: String,           // e.g. "default-8d41c2e07a95b3f6"
    pub previous_shell_dir: String,  // the name `active` named before; "default" when an older ocx rendered last
    pub fingerprint: String,         // render_fingerprint, 64 hex
    pub generation: String,          // LauncherGeneration of the writer
    pub names: BTreeSet<String>,
    pub bin_fingerprint: BTreeMap<String, BinEntryStamp>,   // over shells/<shell_dir>/bin, records included
    pub link_fingerprint: BTreeMap<String, String>,
}
// RenderStamp (v1, render_stamp.json) unchanged and read only by the new ocx.
```

### 8.3 The prompt gate

`active`'s validity is exact, never a name pattern: it must name `shells/<stamp v2 shell_dir>` (v2 gate) or `shells/default` (v1 gate). The gate tries v2 first: stamp present, `active` names its `shell_dir`, `bin_fingerprint` matches → `active/bin` on `PATH`. Otherwise v1, exactly as today (`activation.rs:831-886`): stamp present, `active` names `shells/default`, its fingerprint matches. Otherwise nothing. An unknown `v` reads as absent (`state_store.rs:284-290`).

## 9. Runfiles manifest — limits (item 10)

- Order: tree stages first, manifest last (research file 2, implication 1): only a tree holds declared links, and a remote action never has a usable manifest (research file 2, § 1).
- The record is never looked up through a manifest: it sits beside the started path in every form rules_ocx builds (MR-3, MR-4; rules_ocx `ocx/private/set_rules.bzl:100-118`).
- The root is the directory that holds the record's target. rules_ocx's set files are source files of one external repository (`set_repo.bzl`), so the root holds the whole set. A set with declared links fails on Windows already (C-RO-13). A reserved file name (`BUILD.bazel`) restored by `ocx_set` (`set_rules.bzl:37-44`) lives in `bazel-out`, not under the root, so such a set needs a tree on Windows; the launcher's exec error names the missing program.

## 10. Mixed-version homes and existing installs (items 4, 11)

What an older `ocx` does was read in its code, which is today's: the prompt gate (`activation.rs:831-886`) and `heal_active` (`render_toolchain.rs:1483-1513`).

| Situation | What happens |
|---|---|
| New ocx on a home an older one built | Reads v1 stamps, old trampoline signals (`.exec`, line-two marker) and old `shims/` for GC. Writes `launchers/<G>`, `deferred/<G>`, `shells/default-<random>`, `render_stamp.v2.json`; switches `active`. Never writes into `entrypoints/`, `shims/…`, `shells/default` or `render_stamp.json`, and never deletes them. Old launchers keep working: their scripts and their `.exe` call the frozen command lines this ocx serves |
| Older ocx's prompt after a new render | It reads its own v1 stamp (untouched), finds `active` naming a directory other than `shells/default` (`active_is_valid` fails, `activation.rs:860-862`) and returns no `bin` entry. In `bin` mode a project's tools leave that older shell's `PATH` until the older ocx renders again; `env` mode (the default) composes without trampolines and is not affected. The global session `PATH` entry (`toolchain/active/bin`) is static, and the new trampolines behind it call frozen command lines, so global tools keep working under the older ocx |
| Older ocx's render after a new render | `heal_active` repoints `active` to `shells/default` and renders there in place, as it always did; its v1 stamp is written. The new ocx's prompt then gates by v1 and works. The new ocx's next render finds its v2 stamp's directory intact and re-switches without publishing (§ 6.4 step 3). The two alternate only when both render alternately; every state either leaves is complete |
| Older ocx using a package only a newer one pulled | Its composer adds `<package dir>/entrypoints`, which does not exist, so that package's entry points are not on its `PATH`; its `binaries` work. Marker below |
| Older ocx's `ocx clean` | Never walks `deferred/` and never retires `launchers/<G>` (litter); removes whole package directories as before; never touches toolchain homes |
| Existing published packages | Nothing changes: no metadata field, no OCI change |
| Existing Windows install | Every `.exe` + `.shim`/`.shimref`/`.exec` stays and runs its own old blob. At the next composition `PATH` names `launchers/<G>` and `deferred/<G>/…`; at the next render `active` names a new directory. A captured old path keeps a working launcher until `ocx clean` collects that package |
| Running `.exe` | Never replaced or deleted while published: new generations are new directories, and retirement renames a directory first and leaves it whole when Windows refuses (§ 6.1) |

**Decided under goal-loop authority, 2026-11-26 (no human answered in this run; the owner may revisit):** a newer `ocx` does not also write `entrypoints/` for the packages it pulls. The new launcher there would need the old blob to read `.shim`, which the new `ocx` does not embed. Writing new-kind launchers under the old name gives `entrypoints/` no generation and breaks refresh. The case needs two `ocx` versions in one home, and the tools' `binaries` still work. What an older `ocx` then sees for a package a newer `ocx` installed: it adds `<package dir>/entrypoints`, which does not exist, so that package's entry points are not on its `PATH`; its `binaries` work (table row above).

## 11. Trampoline detection (item 6)

`is_ocx_trampoline(path)` (`crates/ocx_config/src/env.rs:501-547`) answers true when any holds, in this order, each failing open as today:

1. Windows: `path` ends in `.exec`, or a sibling `<stem>.exec` is a regular file (old launchers).
2. Every OS: `path` ends in `.launch`, or the sibling `record_path_for(path)` exists, and that file, read with the bounded regular-file rule (`O_NONBLOCK`, `fstat` on the handle, at most `MAX_PEEK_BYTES`), peeks as `toolchain`. `encode` writes `kind` first, so the field sits in the first bytes of every record ocx writes.
3. POSIX: line two of the first 256 bytes is `TRAMPOLINE_MARKER` (old scripts).

`package` and `deferred` records are not trampolines, so `ocx launcher exec` re-entry is not refused (`env.rs:490`). `peek_kind` ignores every other field, so a trampoline of a later schema is still recognised. `ocx_config` already depends on `ocx_env_apply`. The depth guard of § 3.4 backs this check up.

## 12. Garbage collection (item 8)

Only `ocx clean` retires launcher directories. Every retirement goes through `retire_dir`.

| Directory | `ocx clean` |
|---|---|
| `packages/…/launchers/<G>`, G = this ocx's generation | kept; removed with its package |
| `packages/…/launchers/<G'>`, any other G' | retired, reported as a new `CleanKind::Launchers` (`crates/ocx_cli/src/api/data/clean.rs:17-28`; an added value of an open enum, IC-15, which the compat gate reads as additive) |
| `deferred/<G>/…` | walked by `fs.deferred(&G).list_all()` (marker `l/`), rooted in lock pins; unrooted → `Object` |
| `deferred/<G'>` | the whole subtree retired, `Launchers` |
| `shims/…` (older ocx) | unchanged: rooted in lock pins, collected when unrooted |
| `toolchain/shells/*` of every home named by a v2 stamp (global and `state/projects/<key>/`) | under that home's `toolchain-render` lock, after `refuse_symlinked_home`: every child named `default-<16 hex>`, `.staging-*` or `.gc-*` that is neither the stamp's `shell_dir` nor its `previous_shell_dir` is retired, `Launchers`; `default` and every other name are left |
| `temp/<prefix>-…` scratch with a held lock | skipped (`temp_store.rs:155-158`) |
| `.bin/ocx-shim/*` | not collected (R-U2) |

Limits, stated: a process paused between its start and its record read across a retirement of its directory exits 78 when it resumes; a live shell that hashed a retired path (bash `hash`) fails until it re-resolves; a second `ocx` build with another generation sharing the home sees its directories retired by the first one's clean and recreates them on its next first use. A retirement that Windows refuses leaves the directory whole for a later clean (`Retired::LeftForLater`).

**The deferred staging fix.** `stage_shim_dir` uses `tempfile::TempDir::new_in(temp root)` with no lock (`prepare_lazy.rs:108-122`), so `ocx clean` sees an orphan (`temp_store.rs:159-161`). Staging moves to `fs.temp.scratch`, whose lock file exists before the directory. An older `ocx clean` honours the same lock, because `stale_entries` predates this record.

## 13. Platform details (item 9)

- **macOS signature.** The arm64 blob is linker-signed (`shim.rs:28-52`). Every launcher is a new file written whole from the embedded bytes, never edited in place, and a copy keeps the embedded signature (research file 2, § 4). Quarantine: launchers written by `ocx` carry no `com.apple.quarantine` attribute; a blob or parent carrying one is a case V-L36 checks. Gatekeeper behaviour per inode stays a claim.
- **`ShimBinStore`** gets no new writer: every launcher is a copy of the embedded bytes. Its `path()` suffix question is moot; the store and its test seam stay until no writer remains and are then deleted (internal). Old Windows launchers are hardlinks of files under `.bin/ocx-shim/`, which stay (R-U2).
- **No Unix script on Windows.** `launcher/generate.rs:48-53` and `prepare_lazy.rs:152-165` write one today. A Windows home holds `NAME.exe` and `NAME.launch` only. cmd and PowerShell find `NAME.exe` through `PATHEXT`; MSYS2 and Git Bash run `NAME.exe` when `NAME` is typed: claim, V-L38.
- **Empty `OCX_BINARY_PIN`** is unset (M2).
- **Windows path length.** `<home>\packages\ocx.sh\sha256\1f\<30>\launchers\<16>\NAME.exe` adds 27 characters to today's `entrypoints\`; `deferred\<16>\` adds 17 to today's `shims\`. Vendored package paths fall from 67 to 23 characters (research file 1, § 5).

## 14. Amendments to existing records

The orchestrator applies these verbatim. Each line was read at the cited range.

**`adr_windows_exe_shim.md`**
- `:560`, below the heading "`.shim` Sidecar Format Contract", insert: "Frozen for launchers already on disk; newly written launchers carry `NAME.launch` instead (`adr_unified_launcher.md` § 3)."
- `:573-576` replace "This is the **One-Way-Door artifact**. Once installed packages carry `.shim` files, changing the encoding/terminator/semantics requires regenerating every installed launcher. The format is deliberately minimal (one line, one meaning) to make it durable." with: "The `.shim` format stays frozen for the launchers already on disk: each `.exe` beside one is the blob of the release that wrote it, and nothing rewrites either. New launchers write `NAME.launch` (`adr_unified_launcher.md` § 3), and a format change is a new generation directory (§ 4 there), so no installed launcher is ever regenerated."
- Every sentence stating that the shim probes `SIDECAR_PROBE_ORDER` or reads `.shim`, `.shimref` or `.exec`: append to each the sentence "This describes the launchers already on disk; the launcher of `adr_unified_launcher.md` reads only `NAME.launch` (§ 3.4 there)."

**`adr_package_entry_points.md`**
- `:16`, after "old launchers on disk outlive every OCX upgrade and continue calling whatever `ocx` the user has on PATH.", insert: "Amended by `adr_unified_launcher.md`: launchers live in `<package dir>/launchers/<generation>/`, written on first use; `entrypoints/` written by an older release stays untouched and keeps calling the frozen `launcher exec` line. The command-line contract stays One-Way High."
- `:250` replace "Any alternative (bake `ocx` version → `ocx-0.42 exec`, or bake the `ocx` absolute path) forces launcher regeneration on OCX upgrade. Regenerating means `ocx install --repair` which complicates the workflow." with: "Baking the `ocx` version would force a new launcher on every upgrade. A new launcher is now cheap and automatic for format changes (`adr_unified_launcher.md` § 4), but the command line still names no version, so a generation changes only when the launcher bytes or the record change."

**`adr_toolchain_activation.md`**
- `:159-164` replace the POSIX body block and the line after it with: "A trampoline is the native launcher plus `NAME.launch` of kind `toolchain` (`adr_unified_launcher.md` § 3.3). It removes `OCX_GLOBAL` and `OCX_PROJECT` and runs `<ocx> --project '<abs project root>' exec -- NAME …`, or `--global exec -- NAME …` for the global home, where `<ocx>` is a non-empty `OCX_BINARY_PIN`, else the baked absolute `ocx`, else the first `ocx` on `PATH` outside the launcher's own directory (§ 3.4 there). Scripts an older release wrote stay and keep working."
- `:394-404` replace the heading "**Windows — a third sidecar, `<stem>.exec`.**" and the paragraph, table and bullets under it with: "**Windows — no third sidecar for new launchers.** A trampoline written by this release is `NAME.exe` plus `NAME.launch` of kind `toolchain`, the same pair as on POSIX (`adr_unified_launcher.md` § 3). The `.exec` sidecar of earlier releases (one line: the absolute project root or `global`, optionally the baked `ocx.exe`) stays on disk with its own `.exe`, keeps working, and is still recognised by `is_ocx_trampoline`. The child command line keeps the root-flag-first shape, and `OCX_GLOBAL` and `OCX_PROJECT` are removed from the child."
- `:472` replace "**`bin/` is a whole-directory reconcile at render**: … Directory-level atomic publish is rejected: `move_dir` calls `remove_dir_all` on the destination and is banned by the atomic-publish-with-convergence rule." with: "**Each render with new content writes a new directory** `shells/default-<16 random hex>/bin`, published by one rename, then switches `active` (`adr_unified_launcher.md` § 6.4). The render deletes nothing; `ocx clean` retires directories the stamp no longer names. `move_dir` is not used."
- `:478`, append: "Stamp v2 is a separate file `render_stamp.v2.json`; it records `shell_dir`, `previous_shell_dir`, `fingerprint` and `generation`, and its bin fingerprint covers `shells/<shell_dir>/bin`, records included. The v1 file is never written by this release and stays readable (`adr_unified_launcher.md` § 8.2)."
- `:1037-1040` C-071, append: "`shells/` holds `default` (written by earlier releases) and render directories `default-<16 hex>` (`adr_unified_launcher.md` § 6.4)."
- `:1188-1197` C-079, replace "a **relative** symlink whose target is exactly `shells/<name>`" with: "a **relative** symlink whose target is exactly `shells/default` under a v1 stamp, or exactly `shells/<shell_dir>` of the v2 stamp; no other name is valid, whatever its shape."
- `:1285` C-081 row "A link, any other target", replace the action cell "`symlink::replace_atomic` (atomic on POSIX; the bounded window `symlink.rs:203-224` documents on Windows, R8)" with: "`retarget_link` toward the v2 stamp's `shell_dir`, or toward this render's new directory (`adr_unified_launcher.md` § 6.1, § 6.4)".

**`system_design_toolchain_activation.md`**
- `:373-389` replace the section "Sidecar contents (shared read rules; one new extension)" with: "### Launcher records\n\nA launcher written by this release has one record `NAME.launch` beside it, JSON, at most 1 MiB, `kind` first: `package` (`package_root`), `deferred` (`identifier`) or `toolchain` (`tier`, `project_root`, `ocx`) — the schema, validation and examples are `adr_unified_launcher.md` § 3. The one-line `.exec`, `.shim` and `.shimref` sidecars of earlier releases stay on disk with their own `.exe` and keep working; no release writes them any more."
- `:391-410` replace the section "Launcher bodies" with: "### Launcher bodies\n\nNo body is generated: every launcher is a byte copy of the native launcher blob for the host, on POSIX and Windows alike, written into a new render directory `shells/default-<16 random hex>/bin` (`adr_unified_launcher.md` § 6.4). What it runs per kind, and how it finds `ocx`, is § 3.4 there. The frozen wire tokens `launcher exec`, `launcher shim` and `exec` keep their paired goldens, now between `body.rs`'s token constants and the launcher's command builder."
- `:525-537` replace the `RenderStamp` block with the `RenderStampV2` block of `adr_unified_launcher.md` § 8.2 and the sentence "The v1 `RenderStamp` stays readable and is never written."

**`adr_exec_env_model.md`**
- `:77` D3, append: "A new generation is a new file, never a changed one (`adr_unified_launcher.md` § 4), so D3 holds per file."
- `:872` replace "the Windows `.exec` sidecar `<root>\n<abs ocx>` (`body.rs:111-129`). New frozen surface: the header grammar (C3)" with: "the Windows `.exec` sidecar `<root>\n<abs ocx>`, which old launchers keep and no release writes any more. New frozen surface: the header grammar (C3); the record `NAME.launch` schema 1 (`adr_unified_launcher.md` § 3); an empty `OCX_BINARY_PIN` reads as unset in every launcher."
- `:917` replace "Refresh stays deferred to [#679](https://github.com/ocx-sh/ocx/issues/679)" with: "Refresh is decided by `adr_unified_launcher.md`: generation directories written on first use, and the update hand-off renders the global home."

**`adr_self_update_handoff.md`**
- `:42-43`, after "phases 2–5 write every setup surface using the new version's own code.", insert: "Phase 1.6 renders the global toolchain home when `$OCX_HOME/ocx.lock` exists; its failure warns and never fails setup, because phase 1 already committed (`adr_unified_launcher.md` § 8.1)."

**`adr_vendor_rules_ocx_remote_execution.md`**
- `:17` replace the Reversibility line with: "**Reversibility:** one-way door **High** for the manifest once a release writes it (`vendor-manifest` v2, `adr_unified_launcher.md` R-U4). The record `NAME.launch` is versioned and written with the launcher bytes of the same release: inside a vendored directory it is two-way; in homes it is One-Way Medium, contained by generation directories (`adr_unified_launcher.md`, header). Everything else is two-way (the directory layout is named by the manifest; rules_ocx's surface is pre-1.0 and already breaking)."
- `:77` replace the heading "### Choice 2 — launcher sidecar format (re-scored under R-1)" with "### Choice 2 — launcher record format (re-scored under R-1; the file is `NAME.launch` since `adr_unified_launcher.md`)", and `:88` replace "**Chosen: B** (R-1). The sidecar is a JSON object with `schema_version: 1`; the launcher deserialises it with `serde_json` into types that refuse unknown fields; an unknown version or field exits 78. `ocx_shim` gains `serde` and `serde_json` as dependencies; its crate-map row gains only `ocx_env_apply`. The types live once in `ocx_env_apply::direct` and both the writer and the launcher use them." with: "**Chosen: B** (R-1). The record is `NAME.launch`, a JSON object with `kind: "direct"` and `schema_version: 1`; the launcher deserialises it with `serde_json` into a struct that refuses unknown fields; an unknown kind, version or field exits 78. `ocx_shim` gains `serde` and `serde_json`; its crate-map row gains only `ocx_env_apply`. The types live once in `ocx_env_apply::launch` (`adr_unified_launcher.md` § 3.1) and both the writer and the launcher use them."
- `:136-148` Choice 7: replace the option A header cell with "A. Started path when it ends with `self` → `<P>.runfiles/<entry>` → `$RUNFILES_DIR/<entry>` only when P lies under it → the runfiles manifest (`adr_unified_launcher.md` § 5 step 6); byte equality binds the record", the row "Windows default runfiles (manifest only) (1)" score for A from "1" to "5", A's total from "**90**" to "**94**", and `:148` with "**Chosen: A** (DV-6). Full algorithm: system design C-012; the manifest stage is scored in `adr_unified_launcher.md` M8."
- `:152` replace "a `.direct` launcher kind with a JSON sidecar built for six targets" with "a native launcher built for six targets with a JSON record `NAME.launch` of kind `direct`".
- `:156-161` replace the layout block with:
  ```
  DIR/manifest.json
  DIR/l/NAME[.exe] + NAME.launch                    one per tool of the selection
  DIR/p/<HEX16>/c/…                                 one per package digest, once
  DIR/p/<HEX16>/e/NAME[.exe] + NAME.launch          the package's entry points
  ```
- `:216` replace the risk row "| Windows default runfiles have no tree | launcher exits 78 naming `--enable_runfiles`; rules_ocx docs require it on Windows (DV-6, owner confirmation deferred) |" with "| Windows default runfiles have no tree | the launcher reads the runfiles manifest (`adr_unified_launcher.md` § 5 step 6); `--enable_runfiles` is no longer required |".
- `:270` OQ-34, replace the decision and reason cells with "Done: installed homes use the same launcher and record on every OS (`adr_unified_launcher.md`)" and "owner, second run".
- `:297` DV-6, replace the "This ADR" cell with "started path, `<P>.runfiles`, `$RUNFILES_DIR` only when P lies under it, then the runfiles manifest; Windows needs no `--enable_runfiles`" and the reason cell with "superseded by `adr_unified_launcher.md` § 5".

## 15. The node 26.13.1 defect (R-U1)

Cause (confirmed by the orchestrator in a scratch home): the package's `binaries` claim names `corepack`; the layer's `content/bin/` holds `node npm npx`; Node 25 and later no longer bundle corepack. `ensure_binary_targets_exist` (`tasks/vendor.rs:977-993`) refuses the run with or without `--omit-links` (it runs before the link loop, `:947` against `:957-969`).

Decision: the refusal stays (65 `vendor_path_unrepresentable`). The message names the package identifier and says that its `binaries` claim lists `corepack`, a program the content does not hold, and that the package publisher must correct the claim. `--manifest-only` cannot see content and succeeds where a full run refuses; MR-10 (equal fields) holds for every run that succeeds.

**Owner follow-up:** the package `ocx.sh/nodejs/node` for versions 25 and later must drop `corepack` from its `binaries` claim; it is published from another repository.

## 16. Consumer requirements (rules_ocx ADR 0003 MR-1…MR-14)

| MR | Verdict | Where code changes |
|---|---|---|
| MR-1 `schema_version` 2 | met (R-U4) | `ocx_package::vendor::SCHEMA_VERSION`, golden, schema |
| MR-2 relative `/` paths; no `_ocx_reserved` segment | met; the segment is refused (C-025) | `VendorLayout::check_relative_path`, `RESERVED_SEGMENTS` |
| MR-3 `launcher_path` and `record_path` together, record beside launcher | met (C-026) | `build_manifest`, `VendorLayout` |
| MR-4 record found from the started name; both linked under manifest basenames work | met (C-027, § 5 step 4) | `ocx_shim` lookup |
| MR-5 one selection, root digest a platform manifest | met: selection `default` (`tasks/vendor.rs:400`); two `PACKAGE` arguments give one selection with two roots (`:600-609`); the root digest is the platform-manifest leaf (`:335-355`, `resolve.rs:531-606`) | none |
| MR-6 a bare platform manifest checked against `-p P`, exit 65 when it does not serve P | refused as written (R-U9): vendored for `-p P`, `platform = P`, `available_platforms = ["any"]` (`tasks/vendor.rs:310-312`), no new exit code (C-029) | none; a contract and row V-L59 |
| MR-7 `available_platforms` from the index | met (`tasks/vendor.rs:304-309`, `:262-272`) | none |
| MR-8 the selecting image index | met with a different field name: `packages[].index_digest`, roots only (R-U6, C-031) | `read_closures` keeps `chain[0]` as `SelectedRoot.index_digest`; `build_manifest` writes it |
| MR-9 paths read under `--index` | met with a different field name: top-level `index_lookups[] {path, found}` (R-U7, C-032) | a lookup recorder on `IndexStore` at `read_index_document` |
| MR-10 `--manifest-only` writes the same fields, `content: absent` | met: only `content` and `packages[].tree` differ (`tasks/vendor.rs:138-143`, `:940`, `:946` against `:520`, `:593`); with R-U1 a full run may refuse where `--manifest-only` succeeds (C-028) | none |
| MR-11 `--omit-links` lists every omitted link | met (today; C-028) | none |
| MR-12 `--remote` live, `--frozen --index` exits 81, no write into the index directory | `--remote` and `--frozen --index` met; the no-write half is a defect (R-U10): `place_package` and `verify_selection` use the writable manager (`tasks/vendor.rs:1011-1017`, `:379`) | both call `manager.read_only_view()` (C-030); row V-L60 |
| MR-13 tree, then the runfiles manifest; 78 only when neither | met (R-U5, § 5 steps 5-7) | `ocx_shim` lookup |
| MR-14 `ocx update --no-pull` on a scratch copy | met (`app/project_context.rs:478-489`, `update.rs:95-97`, `:120-122`, `:154-160`, `:271-327`); side effects: a toolchain render at `<copy dir>/.ocx/toolchain` and project state under `$OCX_HOME/state/projects/<key>`, both of which rules_ocx confines to a scratch copy and a scratch `OCX_HOME` (ADR 0003 MR-14); `--no-pull` still resolves live unless `--frozen --index` or `--offline` | none |

## 17. Verification

Every row is shown red once and green once. "Claim" marks a Windows or macOS statement this host cannot prove; the named CI job proves it.

| ID | Check | Red (control) | Green | Where |
|---|---|---|---|---|
| V-L01 | `decode` per kind: a valid record decodes; one extra key, a key of a sibling kind, an unknown key inside an `env[]` entry, a missing `kind`, an unknown `kind`, a duplicate `kind`, a struct `kind` different from the peek and `schema_version: 2` are each refused | `deny_unknown_fields` removed from one struct, or from `EnvRecord` → that case decodes | all refused, all valid ones decode | `ocx_env_apply` unit, `verify-basic.yml › test` |
| V-L02 | `encode` is byte-stable and `kind` comes first | a field moved before `kind` (test seam) → the golden fails | golden equal | `ocx_env_apply` unit |
| V-L03 | `toolchain` tier rules | a check that ignores `tier` → decodes | `TierMismatch` | `ocx_env_apply` unit |
| V-L04 | `@VENDOR_DIR@` outside `direct` refused | the check removed → decodes | `TokenOutsideDirect` | `ocx_env_apply` unit |
| V-L05 | `record_path_for`: `C:\a\cmake.exe` → `C:\a\cmake.launch` (Windows); `/a/foo.exe` → `/a/foo.exe.launch` (Unix) | strip on every OS → the Unix case fails | both | `ocx_env_apply` unit |
| V-L06 | Empty `OCX_BINARY_PIN` resolves to the next rung | today's `pin.or(baked)` → empty token | next rung | `ocx_shim` unit on Linux and `basic-repo.yml › unit-native (windows-latest)` |
| V-L07 | Home kinds: argv equals the frozen token sequence; `OCX_GLOBAL`/`OCX_PROJECT` removed for `toolchain` only | the selector after `exec` → golden fails | golden equal | `ocx_shim` unit; `test_launcher_exec.py`, `test_trampoline_exec.py`, `test_lazy_loading.py` in `basic-repo.yml › acceptance` |
| V-L08 | A launcher beside only `.shim`, `.shimref`, `.exec` or `.direct` exits 78 naming `NAME.launch` | a `.shim` reader left in → runs | 78 | `ocx_shim` unit; `launcher-blobs.yml` Windows legs (claim) |
| V-L09 | Old Windows launchers (`.exe` + `.shim`, `.exec`) placed on disk start their tool through the new `ocx` | the `launcher exec` grammar changed (test seam) → fails | starts | `test_exec_env_compat.py` with Windows bodies, `deep-repo.yml › acceptance-native (windows-latest)` (claim) |
| V-L10 | The `PATH` walk skips empty and relative entries, the launcher's own directory and other launchers; a Unix `PATH` with the launcher's directory first starts the real `ocx` | `execvp("ocx")` → the launcher's directory wins | the real `ocx` | `ocx_shim` unit; `test_launcher_exec.py` |
| V-L11 | Windows: an `ocx.exe` in the working directory and one in the launcher's directory are never started; `lpApplicationName` is always set | the NULL rung kept → the planted `ocx.exe` runs | the real `ocx` | `deep-repo.yml › acceptance-native (windows-latest)` (claim) |
| V-L12 | A relative pin and a batch-file pin, baked `ocx` or found `ocx` are refused on every rung | `is_batch_target` check removed → `ocx.cmd` spawned | 78 | `ocx_shim` unit |
| V-L13 | An entry point named `ocx`, and two packages each with an `ocx` entry point on `PATH`, start the real `ocx` | the "skip other launchers" rule removed → a loop, ended by the depth guard | the real `ocx`, depth 1 | `test_entrypoints.py` |
| V-L14 | A launcher loop stops at depth 32 with 78 | the guard removed → the test times out | 78 | `ocx_shim` unit, `test_launcher_exec.py` |
| V-L15 | A home launcher started through a symlink placed elsewhere reads the record beside its image path; `NAME` is the image's name | record read beside `P` (test seam) → 78 | runs | `ocx_shim` unit with `LookupFs::image_path` |
| V-L16 | Same-bytes switch: two render directories with byte-identical launchers; `active` switched after exec and before the record read (seam) → the record of the loaded directory is used | the record read through `P` (the earlier design) → the new directory's record | loaded directory's record | `ocx_shim` unit |
| V-L17 | A switch between an identity check and the open cannot pair the image with another directory's record | the stat-then-open design (test seam) → mismatch accepted | no compare exists; kernel path used | `ocx_shim` unit |
| V-L18 | Linux: an image path ending in ` (deleted)` exits 78 | the suffix check removed → a missing record path is read | 78 | `ocx_shim` unit |
| V-L19 | Windows `NtQueryVirtualMemory` (mapped file name) → `GetFinalPathNameByHandleW` names the render directory behind the `active` junction; macOS `proc_pidpath` names the loaded directory | `GetModuleFileNameW` (test build) → names `active` | the render directory | `basic-repo.yml › unit-native (windows-latest)` and `unit-native (macos-latest)` (claims) |
| V-L20 | First use: an install by `ocx-previous` gains `launchers/<G>` whole at the next `ocx exec`; `entrypoints/` stays byte-identical | first use writes into `entrypoints/` (test seam) → its tree hash changes | present; unchanged | `test_entrypoints.py` with `ocx-previous` (`test/src/previous_release.py`), `basic-repo.yml › acceptance` |
| V-L21 | A fault after the first launcher leaves no `launchers/<G>`; a retry succeeds | per-file writes into `D` (test seam) → a partial `D` | absent, then whole | `ocx_package_manager` unit |
| V-L22 | Two concurrent first uses converge | `AlreadyThere` treated as an error → one fails | both 0 | `ocx_package_manager` unit |
| V-L23 | A `D` missing one record, and a symlink at `D`, are not trusted: retired and republished | "is a directory → done" → the incomplete `D` is composed | complete `D` | `ocx_package_manager` unit |
| V-L24 | The package directory removed during composition is not recreated by `launchers/` | `create_dir_all` (test seam) → a package directory with only `launchers/` | composition fails, no directory | `ocx_package_manager` unit |
| V-L25 | Read-only home: a complete `entrypoints/` is composed with one warning; without one the composition fails naming the path, 77 | no fallback → fails although `entrypoints/` exists | as stated | `ocx_package_manager` unit (read-only tempdir) |
| V-L26 | Deferred staging survives `ocx clean` | today's unlocked `TempDir` → collected | kept, publish succeeds | `ocx_package_manager` unit |
| V-L27 | A tool named `foo.launch` is refused by `ocx vendor` (65) and skipped with a warning by home writers; two names equal after case folding collapse on Windows and macOS targets | the check removed → `foo`'s record overwritten | refused / skipped | `ocx_package_manager` unit; `test_vendor.py` |
| V-L28 | A render with new content publishes a new directory and switches `active`; a repeated render writes nothing | a render that ignores the stamp fingerprint → a second new directory | one directory; second render no-op | `ocx_package_manager` unit, `test_toolchain_render.py` |
| V-L29 | Neither render nor clean writes, renames or deletes `shells/default`, `entrypoints/`, `shims/` or `render_stamp.json` | retire matching `default` → it is gone | byte-identical old trees | `ocx_package_manager` unit with a seeded older home |
| V-L30 | The render deletes nothing; `ocx clean` keeps `shell_dir` and `previous_shell_dir`, retires older `default-*`, `.staging-*` and `.gc-*`, never a symlink candidate; each retirement ends `Removed` or `LeftForLater` | retire inside the render (test seam) → the previous directory is gone during a start | as stated, both outcomes asserted | `ocx_package_manager` unit; `test_clean.py` |
| V-L31 | Windows in-place retarget: 10000 retargets against a concurrent lookup loop count zero failures, as a standard user; a second concurrent retargeter; an `active` that is a directory symlink is healed | the remove-then-rename path with a test pause between remove and rename → failures counted | zero failures | new job `junction-proof` in `basic-repo.yml` on `windows-latest` (claim; red → the exception of M6 ships and the PR says so) |
| V-L32 | Windows: `ocx clean` with a running `.exe` in a directory leaves the directory whole or removed, never partial | `remove_dir_all` without the rename → a directory without its records | whole or gone | `junction-proof` (claim) |
| V-L33 | Prompt gate: v2 gates `shells/<shell_dir>` only; v1 still gates `shells/default`; any other `active` target is refused; an unknown `v` reads absent | a name-pattern check → a planted `shells/default-0000000000000000` gates | as stated | `ocx_package_manager` unit (`activation`) |
| V-L34 | `ocx-previous` and the new `ocx` render and prompt alternately on one home: every launcher starts; the older prompt omits the project entry only while `active` names a new directory | the new ocx writes the v1 file (test seam) → the older prompt reads a foreign stamp | as stated | `test_toolchain_activate.py` with `ocx-previous`, `basic-repo.yml › acceptance`; Windows `deep-repo.yml › acceptance-native (windows-latest)` (claim) |
| V-L35 | `.staging-<random>` is created exclusively and `refuse_symlinked_home(_leaves)` runs before it | `create_dir_all` (test seam) → a planted symlink is followed | refused | `ocx_package_manager` unit |
| V-L36 | macOS arm64: launchers copied into three directories start and pass `codesign --verify`; with `com.apple.quarantine` on the blob copy and on its parent the start is reported; a one-byte-changed copy fails `codesign --verify` | the changed copy (control) | starts exit 0 | `basic-repo.yml › unit-native (macos-latest)`, `launcher-blobs.yml › Run aarch64-apple-darwin` (claim) |
| V-L37 | Every home launcher is a byte copy of the embedded host blob, not a hardlink; changing one changes no other | hardlink writer (today) → link count > 1 | link count 1, bytes equal the blob | `ocx_package_manager` unit |
| V-L38 | A Windows home holds no extensionless script; `NAME` starts from cmd, PowerShell and Git Bash | a script written beside (today) → listing fails | `.exe` and `.launch` only; three starts | `test_windows_shim.py`, `deep-repo.yml › acceptance-native (windows-latest)` (claim) |
| V-L39 | `is_ocx_trampoline`: true for a native trampoline, an old `.exec` pair, an old marker script and a `schema_version: 2` trampoline record; false for `package` and `deferred`; a FIFO at the record path does not hang | the peek replaced by `decode` → the schema-2 case false | all cases | `ocx_config` unit on Linux and `basic-repo.yml › unit-native (windows-latest)` |
| V-L40 | Two project homes on one `PATH` do not loop | detection disabled (test seam) → loop, timeout | 65 | `test_trampoline_exec.py` |
| V-L41 | `ocx clean` retires a non-current `launchers/<G'>` and `deferred/<G'>` as `launchers`; `--dry-run` only reports; the compat gate accepts the added `launchers` value and reds on its removal | `G` constant (test seam) → nothing retired; value removed from the schema → gate red | as stated | `test_clean.py`; compat gate in `verify-basic.yml › lint` |
| V-L42 | The hand-off renders the global home: after `ocx self update` to a build with a new blob, `active` names a new directory | phase 1.6 removed → the old directory stays | new directory | `test_self_update.py`, `basic-repo.yml › acceptance` |
| V-L43 | Manifest parse: escaped lines, an empty target, a literal backslash, `\` and `/` targets, a line over 64 KiB, a NUL line, a 65 MiB file, a candidate set over 16 MiB of records, duplicate roots deduplicated before the cap | split at the last space (test seam) → the escaped case fails; dedupe removed → 4096 duplicates exhaust the cap | each case as specified | `ocx_shim` unit with `LookupFs` |
| V-L44 | An inherited `RUNFILES_MANIFEST_FILE` that does not list `P` is ignored; one that lists `P` is used; a FIFO at a manifest path does not hang | the "lists P" rule removed → another tree's root is used | own root | `ocx_shim` unit |
| V-L45 | A derived root that is relative, holds `..`, CR or the list separator is rejected | the root check removed → accepted | rejected | `ocx_shim` unit |
| V-L46 | Windows without `--enable_runfiles`: a vendored tool, and one calling another by bare name, start through the manifest; Bazel's copies of the launcher and record sit side by side; the manifest file name and line shape are printed and asserted | the manifest stage removed (test build) → 78 with the hint | both exit 0 | rules_ocx `verify-deep (windows-latest)` with `--noenable_runfiles` (claim) |
| V-L47 | The same on a workspace path holding non-ASCII characters | the manifest read through the ANSI code page (test build) → root not found | exit 0 | rules_ocx `verify-deep (windows-latest)` (claim) |
| V-L48 | Linux local `--noenable_runfiles` starts (research file 1, § 5 measured 78) | the manifest stage removed → 78 | exit 0 | rules_ocx job `test` |
| V-L49 | `bazel coverage` of a target using a vendored tool starts it | the manifest stage removed → 78 where coverage runs without a tree | exit 0 | rules_ocx job `test` |
| V-L50 | Short layout: every path is `l/…` or `p/<HEX16>/…`; every launcher has its `record_path` and the file exists in a full run; the selection environment names the same `p/<HEX16>/c` paths as `packages[]` | the composer's hand-derived tail left in → paths differ | equal; files present | `test_vendor.py`, `test_package_vendor.py` |
| V-L51 | A `HEX16` clash is refused naming both digests | `package_keys` without any clash check → one directory for two packages | 65 | `ocx_store` unit |
| V-L52 | node 26.13.1: the refusal names the package identifier and says its `binaries` claim lists `corepack`, a program the content lacks | today's message → the text assertion fails | message as stated, 65 | `test_vendor.py` |
| V-L53 | The manifest says `schema_version: 2` and validates against the v2 schema | v1 written (test seam) → the schema check fails | 2, valid | `test_vendor.py`; `verify-basic.yml › lint` (schema golden) |
| V-L54 | `index_digest`: present on a root resolved through an image index, absent for a platform-manifest identifier and for dependencies | `read_closures` drops `chain[0]` (today) → absent everywhere | as stated | `test_package_vendor.py` |
| V-L55 | `index_lookups`: with `--index` lists every path read, found or absent, sorted; absent without `--index` | the recorder not attached → empty | as stated | `test_vendor.py` |
| V-L56 | A package content path holding a `_ocx_reserved` segment is refused (65) | `RESERVED_SEGMENTS` empty → listed | 65 | `ocx_store` unit; `test_vendor.py` |
| V-L57 | Each of the six launcher blobs stays within 640 KiB | budget exceeded → the size gate reds | ≤ 640 KiB | `build-windows-shims.yml › build` |
| V-L58 | Each committed blob starts every record kind on its own OS | a blob that accepts any record → the case reds | all cases | `launcher-blobs.yml`, six legs (Windows and macOS claims) |
| V-L59 | R-U9: `ocx package vendor -p P registry/repo@<platform manifest digest>` vendors it with `platform = P`, `available_platforms = ["any"]`, exit 0 | a check that refuses a bare manifest → 65 | as stated | `test_package_vendor.py` |
| V-L60 | R-U10: a vendor run with `--index DIR` leaves `DIR` byte-identical, a digest-addressed package included | the writable manager (today) → a dispatch object written into `DIR` | tree hash equal | `test_vendor.py` |

## 18. NFRs affected

- **Availability.** No published launcher directory is ever partial; a crash leaves the old or the new state. On Windows the `active` switch has no gap once V-L31 is green; until then the exception of M6 applies. Launcher directories are derived state: no parent-directory `fsync` follows a publish, because a rename lost in a crash is redone by the next first use or render.
- **Latency.** Warm composition adds one completeness check per package with entry points and per deferred tool. A home launcher start adds one kernel image-path query and one record read; a `direct` start on the tree path adds nothing; the manifest stage reads a stream until the first accepted line. Trampoline detection adds one bounded read before an exec.
- **Security.** No rung starts a program found by searching the launcher's directory or the working directory; batch programs are refused; home kinds bind the record to the loaded image; an inherited `RUNFILES_MANIFEST_FILE` is used only when it lists the started path, and the byte binding stays; records and manifests are bounded, regular-file only; `active` validity is exact; every launcher is an independent copy. Windows SmartScreen and ASR rules on new unsigned user-directory `.exe` files are a claim with no CI check.
- **Cost.** A copy per launcher (≤ 640 KiB each, about 25 MiB for a 50-tool render directory, two kept per home); one launcher set per generation until `ocx clean`.
- **Operability.** `ocx clean` reports retirements as `launchers`; every launcher refusal names the path it tried; a read-only or foreign-owned home either composes its older `entrypoints/` with one warning or fails naming the path (77 or 74).

## 19. Consequences

**Positive:** one launcher program and one record on every OS; issue 679 closes by construction; Windows Bazel builds need no `--enable_runfiles`; vendored package paths shrink by 44 characters; rules_ocx derives no file name.

**Negative:** every home gets a second set of launcher directories on first use after the upgrade; superseded generations stay until `ocx clean`; an older ocx's `bin`-mode prompt omits project tools while a newer render is active; an older ocx does not see entry points of packages only a newer one pulled (marker § 10); captured launcher paths break after a retirement; disk use grows by a copy per launcher.

**Risks and mitigations:**

| Risk | Mitigation |
|---|---|
| The in-place junction retarget is not atomic or needs a privilege | V-L31 decides before merge; the exception of M6 ships otherwise |
| The Windows image-path resolution does not name the render directory | V-L19; the alternative is `QueryFullProcessImageNameW`, judged by the same row |
| The Windows manifest line shape differs from Linux | V-L46 prints and asserts it; the parser accepts `\` and `/` |
| A launcher grows past 640 KiB | V-L57 |

## 20. Validation

- [ ] Every row of § 17 shown red once and green once; each claim row green in its named CI job
- [x] The two golden values of § 4.1 computed from its byte strings (Python `hashlib.sha256`, orchestrator, 2026-11-26) and inserted
- [ ] `task satellite:verify`: `ocx_env_apply` (ecosystem) renames `DirectSidecar`; ocx-mirror upgraded in the same series if it names it
- [ ] Security review of the launcher lookup (§ 5) and program resolution (§ 3.4) by the security-auditor skill

## Links

- [`system_design_vendor_rules_ocx.md`](./system_design_vendor_rules_ocx.md), [`adr_vendor_rules_ocx_remote_execution.md`](./adr_vendor_rules_ocx_remote_execution.md)
- [`adr_windows_exe_shim.md`](./adr_windows_exe_shim.md), [`adr_package_entry_points.md`](./adr_package_entry_points.md), [`adr_toolchain_activation.md`](./adr_toolchain_activation.md), [`system_design_toolchain_activation.md`](./system_design_toolchain_activation.md), [`adr_exec_env_model.md`](./adr_exec_env_model.md), [`adr_self_update_handoff.md`](./adr_self_update_handoff.md)
- `.agents/research/research_launcher-atomic-publish-and-runfiles.md`, `.agents/research/research_launcher-platform-primitives.md`
- rules_ocx `.agents/adr/adr_0003_package_locking.md` (MR-1…MR-14)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-11-26 | architect worker | Initial draft (Proposed) |
| 2026-11-26 | architect worker | Review round 1 (spec, quality, security, SOTA, codex): exact hash inputs with two goldens; per-kind record structs; kernel image path for home kinds; launcher-resolved `ocx` with no directory search and a depth guard; record lookup through a manifest removed, `$RUNFILES_MANIFEST_FILE` kept under the "lists P" rule (R-U5) with manifest hygiene; copies instead of hardlinks; uniquely named render directories with fingerprint and previous directory in a separate v2 stamp; render deletes nothing, `ocx clean` retires; exact `active` validity; no runtime fallback for the junction switch; completeness and name checks; mixed-version behaviour read from code; manifest v2 with `index_digest` and `index_lookups`; consumer requirements MR-1…MR-14; rulings R-U1…R-U10; literal amendment texts; one verification ID space |
| 2026-11-26 | orchestrator | Spec re-validation fixes: M7 and M11 totals corrected; Windows manifest decoding stated (UTF-8 once at the root, never the ANSI code page) and V-L47's red control changed; golden values of § 4.1 computed and inserted; `T0` on a first render; the `LeftForLater` outcome in § 6.2 named |
