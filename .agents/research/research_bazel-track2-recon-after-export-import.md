# Research: Bazel track recon after export/import

## Metadata

**Date:** 2026-11-23
**Lane:** codebase recon (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** origin/main at 19f2fa9f, read through git
**Expires:** 2027-02-23
**Caveat:** findings only, no recommendation

All paths repo-root-relative at `origin/main`. Line numbers are lines of the file at that revision. UNVERIFIED marks what was not read or not run.

## 1. What shipped

Commits: ad1000ea (feat), 8426cf1a (design, plan, evidence), 19f2fa9f (unrelated patch fix, tip).

### Commands

| Command | Source | Flags |
|---|---|---|
| `ocx package export PACKAGE...` | `crates/ocx_cli/src/command/package_export.rs:13-30` | `-o/--output DIR` (required), `-p/--platform` (repeatable; default every platform published), `--referrers` / `--no-referrers` |
| `ocx package import DIR [PACKAGE...]` | `crates/ocx_cli/src/command/package_import.rs:13-28` | `-p/--platform` (repeatable; default host), names narrow the set |
| `ocx export` (root, project lock) | `crates/ocx_cli/src/command/export.rs:14-30` | `-o/--output` (required), group selection (`-g`, default every group), `-p/--platform` (default every platform the lock records), `--referrers`/`--no-referrers` |
| `ocx import DIR` (root) | `crates/ocx_cli/src/command/import.rs:13-27` | group selection (default group only), `-p/--platform` (default host) |

Root commands read the project from `--project <PATH>` or `--global` (`website/src/content/docs/guides/export-and-import.mdx:93`). Wired at `crates/ocx_cli/src/command.rs:419,431,522-523` and `crates/ocx_cli/src/command/package.rs:195,211,540-541`.

`--referrers` / `--no-referrers`: `crates/ocx_cli/src/options/export_referrers.rs:8-29`.

### New global flags, env vars, config keys (exact spelling)

| Surface | Spelling | Source |
|---|---|---|
| flag, repeatable, read-only | `--blob-dir <PATH>` | `crates/ocx_cli/src/app/context_options.rs:132` (doc 123-131) |
| flag, one, writable | `--blob-cache-dir <PATH>` | `crates/ocx_cli/src/app/context_options.rs:139` |
| env, `PathList` (`:` Unix, `;` Windows), max 32 | `OCX_BLOB_DIR` | `crates/ocx_env/src/vars.rs:137`; cap `crates/ocx_config/src/exec_state/policy.rs:12` |
| env, `Path` | `OCX_BLOB_CACHE_DIR` | `crates/ocx_env/src/vars.rs:144` |
| config table `[blobs]` | `dirs` (list), `cache_dir` | `crates/ocx_config/src/blobs.rs:23,29` |
| existing, used to attach an export's pins | `--index` / `OCX_INDEX` | `crates/ocx_cli/src/app/context_options.rs:121` |

Precedence for dirs and cache: flag, then env, then config (`crates/ocx_cli/src/app/blob_settings.rs:85-129`). A flag replaces the whole env/config list. A `--blob-dir` that does not exist exits 79 `blob_dir_not_found` (`blob_settings.rs:53-57,88-93`); an env or config dir that is missing warns once and is skipped (`crates/ocx_oci/src/blob_lookup.rs:109-122`). A relative `[blobs]` path is refused (`blob_settings.rs:132-139`). Both settings are forwarded to child ocx processes (`crates/ocx_cli/src/app/env_flags.rs:18-19`; `vars.rs:135-136,145-146` doc).

No shorthand flag exists (matches the owner's 2026-11-22 drop).

### Export directory layout

```
DIR/ocx-export.json                         marker, written LAST (export.rs:751, test export.rs:1040)
DIR/index/<slug>/p/<ns>/<pkg>.json          package roots (restricted index; absent for root `ocx export`)
DIR/index/<slug>/.../o/...                  dispatch objects (deeper, under o/)
DIR/blobs/<slug>/<alg>/<2hex>/<30hex>/data  every blob
DIR/blobs/<slug>/<alg>/<2hex>/<30hex>/digest        recovery file naming the full digest
DIR/blobs/<slug>/<alg>/<2hex>/<30hex>/referrers/<alg>-<hex>   empty marker per referrer
DIR/blobs/<slug>/<alg>/<2hex>/<30hex>/referrers/.complete     listing was read in full
DIR/blobs/.tmp/                             writer staging, never read
```

- Layout doc: `test/src/export_dir.py:1-5,18-28,52-54`; writer `crates/ocx_oci/src/blob_lookup/writer.rs:4-8,346-355`; markers `crates/ocx_oci/src/blob_lookup/markers.rs:4-7`.
- Marker `ocx-export.json`: `{"format_version":1,"registries":{"<slug>":"<registry as written>"}}`; file name `crates/ocx_oci/src/blob_lookup/export_marker.rs:15`, schema `:24-29`, cap 64 KiB `:18`, unknown fields tolerated `:23`. A cache dir keeps the same marker, advisory (`writer.rs:453-461`).
- Contents of an export: image index, manifests, configs, layer archives of every platform plus dependency closure (`guides/export-and-import.mdx:30-33`; `tasks/export.rs:429-500`), referrers by default (`tasks/export.rs:463-483`), an active patch snapshot's companions and descriptors for the root form only (`tasks/export.rs:155-157,292-330`).
- Root export writes no `index/` (`tasks/export.rs:51-52`; import `tasks/import.rs:381`; guide line 93).

### Blob lookup order

Home blobs, then each `--blob-dir` in list order, then the blob cache, then the network. Every read hashes the bytes. A bad copy in a directory never ends the walk; if no clean copy exists and a directory held a bad one the verdict is 65 `digest_mismatch`; a bad cache entry is removed and counts as a miss.
- `crates/ocx_oci/src/blob_lookup/transport.rs:69` ("Digest reads try the home, then the lookup, verified. Tag reads are not found; pushes are refused."), `blob_lookup.rs:88-90,155-162`, `blob_lookup/stream.rs:58-64,95-151,185-201`.
- Prose: `website/.../guides/export-and-import.mdx:109-113`.
- The index never chains: a tag resolves only from the local index; a blob directory alone never answers a tag (`guides/export-and-import.mdx:113`; test `test/tests/test_blob_cache.py:379`).
- The cache is written after a network fetch (layer archives and metadata); a blob-dir hit is not copied into the cache (`test_blob_cache.py:235-268`; guide line ~140).

## 2. Old Bazel-track decision lines vs what shipped

Discussion: `.agents/discussions/bazel-hermetic-package-bundles.md`.

| Old line | Status | Evidence |
|---|---|---|
| "bundle mode" in rules_ocx (disc. 100, 120) | Not built (rules_ocx is external). On this repo: stale wording, "bundle" is ruled out as the feature noun (disc. 143; ADR `.claude/artifacts/adr_package_export_import.md:~27` Vocabulary). Shipped nouns: export directory, blob directory, blob cache. | `adr_package_export_import.md` Metadata/Vocabulary |
| `LayoutTransport` must serve a blob by digest without `index.json` (disc. 88) | Stale wording. No `LayoutTransport` exists; `git grep LayoutTransport origin/main -- crates` found nothing. The OCI image layout was withdrawn (ADR:65-66, 536). Replacement is `LookupTransport` / `BlobLookup`, keyed by (registry, digest), no index lookup. | `crates/ocx_oci/src/blob_lookup/transport.rs:64-79`; ADR:519 states the Bazel need is exactly `blobs/<slug(registry)>/<alg>/<2hex>/<30hex>/data` |
| `ocx.project(name, ocx_toml, ocx_lock, config, bundle)`; `bundle` label (disc. 69, 76) | Not built here. `ocx.project` on main takes `name`, `ocx_lock`, `ocx_toml` only. rules_ocx source is outside this repo: UNVERIFIED whether 0.5.1 has `config`/`bundle`. The word "bundle" is stale; shipped attach point is a blob directory plus an optional `--index`. | `MODULE.bazel:143-148` |
| "`ocx export` carries the frozen patch set when one exists" (disc. 109) | Matches, with a different trigger wording. Root export (no named packages) exports companions and descriptors of the *active patch snapshot*, and root import assembles them. Whether "active snapshot" equals "`patches.snapshot.json` beside `ocx.lock`" was not traced: UNVERIFIED. | `tasks/export.rs:155-157,292-330`; `tasks/import.rs:163-229`; scenario S-025 in `system_design_package_export_import.md` |
| "In bundle mode the fetch action takes exactly the blobs its package needs, as labels" (disc. 88) | Partly supported, partly missing. Supported: blob path is a pure function of (registry, digest), so a label can name one blob file. Missing: no command lists the digests a package needs (see Q6); a rule must read the leaf manifest JSON from `data`, then config, then layers, plus the dependency closure from config metadata. | `blob_lookup.rs:70-86`; `tasks/export.rs:493-500` |
| "air-gapped workspace hands the bundle to rules_ocx as a label" (disc. 76) | Layout is a plain directory, so a label to a directory works in principle; the consumer in ocx is `--blob-dir`/`OCX_BLOB_DIR` pointing at `DIR/blobs`. UNVERIFIED in rules_ocx. Root `ocx export` is the "bazel run target produces it from ocx.lock" primitive (`command/export.rs:14-60`). | as cited |
| "A bundle with one changed byte is refused (exit 65)" (disc. 284) | Matches. | `test/tests/` S-003 in system design; `stream.rs:185-201` |
| `oras cp --from-oci-layout` reads the bundle (disc. 287) | Stale, already marked as falling away. Not supported; a later `--format oci-layout` export is a named non-goal. | ADR:520; system design:603 |
| "no manifest of contents / does a bundle carry a list" (disc. 140) | Shipped: no list of what a directory holds. The only metadata is `ocx-export.json` (registry map) and, for a package export, `index/`. | `export_marker.rs:24-29` |
| Cache directory opt-in, never an index (disc. 124-130) | Matches. | guide `export-and-import.mdx` "Share a blob cache in CI"; `writer.rs:357-361` |

Decisions not touched by export/import: native launcher, toolchain registration, core "materialise digest D for platform P into a directory" verb, environment report. Each is still not built (Q3).

## 3. Does any command write one package's content tree into a caller-named directory without home state?

No. Found none among the 93 files of `crates/ocx_cli/src/command/` (list read via `git ls-tree`); no `materialize`, `extract`, `unpack` or `--dest` command.

Closest primitive: `ocx package pull` (`crates/ocx_cli/src/command/package_pull.rs:10-19,34-60`): "populates the content-addressed object store", no install symlinks, reports `info.dir().root()`, the package root holding `content/` and `entrypoints/`. It writes OCX home state: `blobs/`, `layers/`, `packages/`, index pin and refs (home layout per `website/.../in-depth/storage` and the pull doc; exact set of directories created under a throwaway `OCX_HOME` not run: UNVERIFIED).

Other home-writers, for contrast:
- `ocx package export` reads the home blobs as a lookup root (`tasks/export.rs:216-219`), takes an export lock under the home `locks` dir (`:121-128`), and a first-sight tag commits a pin to the home index (test `tasks/export.rs:998`, S-031).
- `ocx package import` extracts layers into the home (`tasks/import.rs:79-90,688`) and archives stay in the export.
- `ocx package create --extract` (`command/package_create.rs:94-105`) unpacks into a temp dir for packaging, the opposite direction.

The library has the pieces (`PackageManager::pull` with a `blob_lookup` view, `import_view` at `tasks/import.rs:530-562`) but all land under `file_structure()`.

## 4. Could a program other than ocx assemble a valid blob directory?

Path rule (read side):
`<root>/<slug(registry)>/<alg>/<hex[0..2]>/<hex[2..32]>/data`
- `blob_dir_path` / `blob_data_path`: `crates/ocx_oci/src/blob_lookup.rs:70-86`.
- Shard: `crates/ocx_oci/src/cas_path.rs:10-24` (algorithm, 2 hex, next 30 hex; total 32 hex = 128 bits; "the digest is not recoverable from it").
- Slug: `crates/ocx_util/src/string_ext.rs:6-9,18-19,26` keeps `[a-zA-Z0-9._-]`, every other character (including `:` and `/`) becomes `_`. `localhost:5000` becomes `localhost_5000`.
- Registry is the logical registry of the identifier as written, never the mirror host (`transport.rs:66-68`; discussion 2026-11-22 ruling).
- Algorithms: sha256, sha384, sha512 (`writer.rs:37-67`).

Evidence for hand-assembly:
- The read side needs only the `data` file: it opens "the first regular `data` file" (`stream.rs:90-95,110-123`), hashes it, nothing else. No marker, no `digest` file, no `index/` is consulted for `--blob-dir` reads. `git grep read_marker` shows callers only in export, import and the cache writer (`export.rs:132`, `import.rs:281`, `writer.rs:504`).
- A Python test helper already builds the same path by hand: `test/src/export_dir.py:24-28`.
- Home and `--blob-dir` follow symlinks (`stream.rs:111`), so a directory of links into any content-addressed store works for reading.
- Optional files: `digest` (a recovery aid used only by the writer to detect a shard collision, `writer.rs:168-184`); `referrers/<alg>-<hex>` empty files and `referrers/.complete` (`markers.rs:4-7,55-66`), needed only for offline signature verification.

Evidence against / checks that would reject a hand-built directory:
- Every blob is hashed on read; a wrong byte is 65 `digest_mismatch` (`blob_lookup.rs:164-206`, `stream.rs:185-201`).
- A metadata read has caps: manifest 1 MiB, other whole-blob read 64 MiB (`transport.rs:21-24`); import referrer reads 64 MiB (`import.rs:255-256`); marker 64 KiB (`export_marker.rs:18`).
- Refused registry slugs: empty, `.`, `..`, `.tmp` (`blob_lookup.rs:78-81`); for a cache also a slug equal to `ocx-export.json` (`writer.rs:447-451`).
- `ocx import` (not `--blob-dir`) requires the marker: missing gives `export_dir_invalid` exit 65 (`tasks/import.rs:278-295`). Marker rules: `format_version` must be 1, each key must equal `to_relaxed_slug(value)`, key `locks` reserved (`export_marker.rs:70-103`). Import also walks `index/` (symlinks refused, 65536-entry cap, directory names must be marker registries): `import.rs:303-333,865-915`. Package import needs `index/`; root import needs none (`import.rs:381`; guide line 93).
- A cache directory refuses symlinked ancestors; a `--blob-dir` does not (`stream.rs:111-120`).
- Case-insensitive file systems: two registries folding to one slug are `SlugCollision` (`export_marker.rs:124-141`).
- ocx's own reader treats the on-disk format as one-way: ADR Metadata table (one-way door for layout).
- Whether the `.tmp/` directory or extra files make a directory "invalid": not checked on read (UNVERIFIED for `--blob-dir`; the cache test asserts no stray files, `test/tests/test_blob_cache.py:~120-124`).

## 5. Digest-pinned pull, blob dir attached, no network: blobs alone enough?

Yes for a leaf digest; no `index/` is needed.
- Test: a lock-pinned `ocx pull` on a fresh home with only a warm cache dir (same layout as a blob dir) ends with zero registry requests, and the cache holds at least manifest, config and layer: `test/tests/test_blob_cache.py:145-176` (the assertion text "Manifest, config and layer: the cache holds metadata as well as layers").
- A project export has no `index/` and import works from `DIR/blobs` alone: `tasks/import.rs:116-157` (comment "Writes no index pin"), `command/export.rs:51-52`, guide line 93.
- Import builds an offline view with no index source, `ChainMode::Offline`, `OverlayMode::LookupOnly` so a missing leaf fails instead of dialling: `tasks/import.rs:530-562`.

Blob kinds by case:
- Lock pin or `name@sha256:<leaf manifest digest>`: leaf image manifest, config, every layer archive. No image index. Dependencies add the same set per dependency (`tasks/export.rs:493-500`).
- Digest naming an image index, or a tag: the image index is also needed to select the platform (`tasks/import.rs:620-639`); a tag also needs the local index (`test_blob_cache.py:379`).
- Referrers (signatures, SBOMs, attestations): needed only when a trust policy verifies; verification is lookup-only on import (`tasks/import.rs:707-723,730-807`; system design S-005). A plain pull without verification needs none.
- Layer archives live only in a blob dir or cache, never in the home (ADR Context).

UNVERIFIED: a plain (non-import) `ocx package pull <digest-pinned> --blob-dir D --offline` with an *empty* home was not run; the evidence is the cache variant of the same lookup (test above) and the import path.

## 6. Per-package granularity for build-system inputs

- No command lists the blob paths a digest+platform needs without fetching. `ExportReport` names output dir, per-package rows with platform manifest digest and `written`/`present`, referrer count, blob count, written size: `crates/ocx_cli/src/api/data/export.rs:62-112`. No blob path, layer digest or config digest.
- `ocx package inspect` (`--format json`) reports, for a single manifest, `layers` (digest, media type, size) and with `--resolve` the resolution chain digests (index, manifest, config): `crates/ocx_cli/src/api/data/package_inspect.rs:49-79,365-403,529-540`; command doc `command/package_inspect.rs:10-19`. It reads manifests through the index and client; whether it runs fully offline against only a `--blob-dir` was not run: UNVERIFIED. The `Locked` body projected from `ocx.lock` fetches nothing but carries no layers (`package_inspect.rs:61-64`).
- `--closure` adds the dependency closure with digest-pinned identifiers (`package_inspect.rs:~84-110`).
- A rule can compute a file path from any digest via the rule in Q4, so the missing piece is only the digest set: leaf manifest JSON gives config and layers; config metadata gives dependencies (`tasks/export.rs:485-500`).
- Package-level export into a dedicated per-package directory is possible with existing flags (`ocx package export -o DIR1 pkg@sha256:... -p linux/amd64`, exactly one platform for a digest identifier: `tasks/export.rs:104-111`, guide line 38) but each run takes a home lock and may commit a home pin (Q3).
- ADR names the Bazel need as exactly this path, "with no index lookup": ADR:519.

## 7. `research_bazel_downloader_oci_blobs.md` (2026-11-08; expires 2027-05-08)

Source: `.claude/artifacts/research_bazel_downloader_oci_blobs.md`.

Findings, quoted briefly:
- Fit: "`repository_ctx.download_and_extract(url, sha256=<layer digest hex>, canonical_id=<digest>, type=...)` fits an OCI layer blob: the registry digest of the compressed blob is its sha256" (line 13).
- Auth: "The downloader does no bearer-token challenge" (13, 37); rules_oci exchanges the token in the repo rule (38-41); Tweag credential-helper OCI provider is the alternative (42).
- Redirects: followed; "[#29660] (open, P3, 9.1.0) reports that `Authorization` is forwarded across cross-host and HTTPS to HTTP redirects. Fix status is unknown." (43). Presigned storage URLs may reject the header (44).
- `--downloader_config` has rewrite/allow/block; "No public example rewrites `/v2/<repo>/blobs/<digest>`" (47-50).
- Repository cache: checksum hit succeeds with the URL unreachable if `canonical_id` matches (78); `bazel vendor` stores trees, not blobs (28).
- Prior art: rules_oci downloads per blob, keeps layers compressed, never extracts (54); rules_img lazy layers (55); "No ruleset found unpacks multi-layer OCI content natively into a repo" (57).
- Windows tar-symlink behaviour undocumented (65-69).

Verified vs assumed (the file's own caveat, line 9: "items marked UNVERIFIED had no primary source read"):
- Read from primary sources (docs, source, proposals): `download*` semantics, credential helper protocol, `--downloader_config` directives, rules_oci `authn.bzl` and `pull.bzl`, Bazel flag reference.
- Marked UNVERIFIED in the file: credential helper re-invoked for the redirect host (45); `.wh.` whiteouts, cross-layer hardlinks and overwrite order under extraction (63); `--nofetch` semantics (79); DataDog rules_oci_bootstrap not read (56); #29660 fix status (43).
- Its two proposed spikes (redirected blob GET with `auth=` on ghcr.io and Docker Hub; overlay fixture on Linux and Windows) are recommendations, not results (84-87). No run is recorded in this file.
- It pre-dates export/import: it does not consider reading a blob directory instead of a registry. A hand-built directory leaves the auth and redirect risk out of rules_ocx (no network), but extraction-semantics risk (whiteouts, hardlinks) is unchanged and unverified: ocx's own layer extraction behaviour was not compared.

## 8. How this repository consumes rules_ocx on main

- `MODULE.bazel:25`: `bazel_dep(name = "rules_ocx", version = "0.5.0")`.
- `MODULE.bazel:143-149`: `ocx = use_extension("@rules_ocx//ocx:extensions.bzl", "ocx")`, `ocx.project(name = "tools", ocx_lock = "//:ocx.lock", ocx_toml = "//:ocx.toml")`, `use_repo(ocx, "tools")`.
- Comments `MODULE.bazel:115-141`: ocx CLI fetched sha256-pinned into `@ocx_tool//:ocx`; tool content is not a Bazel action input (a launcher execs an absolute `$OCX_HOME/packages/.../<digest>/content/<bin>` path); "There is no remote execution on this path, ever"; only executables exposed, no route to package data; "Nothing ocx can package may be fetched with `http_file`/`http_archive`" with one carve-out (the Nerd Font).
- Version drift: `.agents/research/research_bundle-rules-ocx-current-state.md:7,15` read rules_ocx 0.5.1 (clone at e311b99) and notes this repo pins 0.5.0.
- rules_ocx source itself (the `bundle`/`config` attributes, `dist_manifest`) was not read in this lane: UNVERIFIED beyond that earlier recon.

## negative

- No command materialises a content tree into a caller directory without home state (Q3).
- No command or report lists the blob paths or digests a package needs without reading manifests (Q6).
- `LayoutTransport`, `fetch_plan`, `bundle_path`: absent in code (ADR withdrew them).
- `oras`/`skopeo` cannot read an export; oci-layout export is a named non-goal (ADR:520).
- Root `ocx import` defaults to the default group and host platform, unlike export (every group, every platform): a Bazel-driven import needs `-g`/`-p` set to match (`guide:91`).
- Every `ocx package export` / `ocx export` run opens the home (lock dir, possible first-sight pin commit); not hermetic for a Bazel action (Q3).
- Layer `data` files are the compressed archives with media-type-dependent compression, under a name with no extension; extraction needs an explicit `type` (research file line 26 for `download_and_extract`; for a local `repository_ctx.extract` the same `type` need: UNVERIFIED).

## leads

- rules_ocx source read at 0.5.1+: which attributes and extension tags exist today (`bundle`, `config`), and whether any consumes a directory label. Reason: Q2 rows for `ocx.project` are UNVERIFIED.
- Run a throwaway `OCX_HOME` pull of one lock pin with only `--blob-dir` and `--offline`, then list what the home gained. Reason: sizes the "home state" cost of the closest primitive and proves the Q5 empty-home case.
- Check `ocx package inspect --resolve --closure` offline against a blob dir only. Reason: it is the only existing report naming layers and chain digests.
- Compare ocx layer extraction (whiteouts, hardlinks, symlinks, file modes) with Bazel `repository_ctx.extract`/`download_and_extract` on the same layers. Reason: the 2026-11-08 research left it UNVERIFIED and it decides whether Bazel can unpack layer blobs itself.
- Trace `patch_snapshot()` to confirm the "frozen patch set" trigger equals `patches.snapshot.json` beside `ocx.lock`. Reason: Q2 row 4 wording.
- Determine whether the 128-bit shard truncation plus `digest` file has any consequence for Starlark path computation (collision refusal `writer.rs:168-184`). Reason: a rule computing paths from digests cannot recover the full digest from the path.
