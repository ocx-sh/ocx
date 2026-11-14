# Research: Signing dogfood, codebase state

## Metadata

- Date: 2026-11-13
- Lane: codebase recon (read-only; no recommendation, no side taken)
- Marker: "(inferred)" = deduced from code or comments, not read as a statement of fact.
- Read tooling: `git show origin/main:<path>` for ocx; working trees for the rest. A `rtk`/shell proxy mangles some `git` output (`index` showed two different HEADs via `git log` vs `rev-parse`); the SHAs below come from `rev-parse` / python subprocess.

| Repo | Commit read | Branch / state |
|---|---|---|
| ocx | `origin/main` = f5e6e66a47c210cdd1791dda0f08e93d39a97d17 | checkout on `goat`, 0 commits ahead of origin/main |
| ocx-mirror (`ocx-sh/ocx-mirror`) | 050aab5bb44428b24e57d7c1cf925264a82e1d96 | `main`, in sync |
| ocx-mirror-sdk (`ocx-sh/ocx-mirror-sdk`) | 98f1cf37e9f80d7eb3e24c1ec71faf7bebfd9406 | `fix/docs-site-url`; `pyproject.toml` modified, one untracked file |
| ocx-indexbot (`ocx-sh/indexbot`, PyPI `ocx-indexbot`) | c7434553d5d85872253c9a9cc73416332a349e5c (tag v0.6.3) | `main`, local is 14 commits behind origin/main (a79f7f3); I also read origin/main for `publishers` |
| ocx-catalog (`ocx-sh/catalog`, npm `@ocx-sh/catalog`) | d560f56bb833900048e85324a6e24aa6ca13b572 (v0.6.3) | `main`, 2 behind origin/main (theme bump, lychee) |
| index (`ocx-sh/index`) | f8c3b28f76d0bcb4a83a42ca309abb8180413492 | `main`, **329 commits behind origin/main (6c67672b)**; `bot-tools/pyproject.toml` modified |
| index-template (`grimoire-rs/index-template`) | ff71715e090505fa85483e80dddf0e3ae6416d58 | `main`, in sync |
| mirror-pypi (`ocx-contrib/mirror-pypi`) | c8f5cf8e53ccc5122830306247d51650a440e02b | `main`; `ocx.lock`/`ocx.toml` modified |
| e2e-mirror (`ocx-contrib/e2e-mirror`) | a4c9fc67afb59bf9d0b8ef26119d402e18b11471 | `main`, in sync |
| ocx-mirror-e2e (remote `michael-herwig/ocx-mirror-cmake`) | dc345496f7c85df9bdf0e71271b4c3ee52577fb6 | `main`; one untracked file |
| worktree e2e-package (`ocx-sh/e2e-package`) | 11f569c0abb50297e87023419f89a83cdaa8af12 | `main` |
| worktree e2e-index (`ocx-sh/e2e-index`) | e8437d5ba731a9a9676a83942c1efc6c8a65a5d4 | `main` |

---

## 1. ocx (origin/main f5e6e66a4)

### 1.1 Publishing workflows (Q1)

Paths are `ocx@f5e6e66a4:.github/workflows/...`; line numbers are the file's own.

| Workflow | What it publishes | Trigger | Shape |
|---|---|---|---|
| `oci-publish.yml` (reusable, `workflow_call`) | OCX package `ocx/cli` (and named variants) to a registry | called only | **One job** `publish` (`:87-89`), loops variants x targets inside one shell step (`:293`, `:311`); `ocx package create` + `ocx package push --platform <p> --cascade` per target (`:347-363`) |
| `post-release-oci-publish.yml` | calls `oci-publish.yml` for `ghcr.io/ocx-sh/ocx/cli` (logical `ocx.sh/ocx/cli`) | `workflow_call`, wired as cargo-dist `post-announce-jobs` (`dist-workspace.toml:38`) from `release.yml` on a version tag; skipped for `-` pre-release tags (`:24-25`, `:42`) | six targets: musl x86_64/aarch64, darwin x2, windows x2 (`:58`); `announce: true` (`:52`), `environment: ocx.sh` (`:59`) |
| `deploy-dev.yml` | `dev.ocx.sh/ocx/cli:<core>-dev_<TS>` | `workflow_dispatch` only (`:14-15`); builds then calls `oci-publish.yml` (`:183-196`) | same six targets |
| `sdkgen-publish.yml` | OCX package `ghcr.io/ocx-sh/ocx/sdkgen` | cargo-dist post-announce, `workflow_call` (`dist-workspace.toml:38`) | `compute-tag` `:41`, `build` matrix of 5 targets `:57-120`, `publish` (one job, loops platforms `:181-203`) `:122`, **`sign`** `:210-257` |
| `docker-publish.yml` | container images `ghcr.io/ocx-sh/ocx` (trixie, alpine; amd64+arm64) | `workflow_call` from release, weekly `cron "0 7 * * 1"`, `workflow_dispatch` with optional `tag` (`:15-30`) | not an OCX package |
| `build-windows-shims.yml` | committed shim blob | n/a | attests provenance, `:105-106`, `:249-250` |

- Tag shape for `ocx/cli`: `<version>_<UTC timestamp>` frozen once in `compute-tag` so every platform push lands on one tag (`post-release-oci-publish.yml:30-39`), plus `--cascade` rolling tags. Each push merges one platform into the tag's image index (so the index digest changes with each platform; `sdkgen-publish.yml:199-200` says so).
- **`oci-publish.yml` has no `permissions:` block on purpose** and the callers grant scopes (`:78-84`). `post-release-oci-publish.yml:15-20` grants `contents: read` + `packages: write` only; `dist-workspace.toml:41` repeats that for `post-release-oci-publish` (no `id-token`). Nothing in the `ocx/cli` publish path can mint an OIDC token today.
- **Signed or SBOM today:**
  - `ocx/cli` and `ocx/mirror`-style publishes: signed = no; SBOM attached = no (no `--sign`, `--sbom`, `attest` anywhere in `oci-publish.yml`).
  - `sdkgen`: **signed**, only the image index of `ocx/sdkgen`, by `ocx package sign --no-tty "${SDKGEN_REPOSITORY}@${INDEX_DIGEST}"` (`sdkgen-publish.yml:257`). The `sign` job is the only one with `id-token: write` (`:219-222`), runs no checkout, uses the ocx from the release build (`:224-241`), and is guarded `github.repository == 'ocx-sh/ocx'` (`:213`). No SBOM on sdkgen.
  - `docker-publish.yml`: buildx `provenance: mode=max` and `sbom: true` (`:199-201`) plus `actions/attest-build-provenance` with `push-to-registry` (`:206-211`); job perms `id-token`, `attestations`, `artifact-metadata` (`:86-88`). This is container images, GitHub attestation format, not the OCX referrer shape.
  - `build-windows-shims.yml`: SLSA provenance on the committed blob (`:249-250`).
  - `release.yml:214-257`: `cargo cyclonedx` writes `.cdx.xml` per workspace crate into the GitHub Release assets (`dist-workspace.toml:48` `cargo-cyclonedx = true`). **`release.yml:257` reads `steps.cargo-cyclonedx.output.paths` (`output`, not `outputs`)**; GitHub resolves an unknown property to empty, so that line adds nothing to the artifact upload (inferred; the `mv` into `target/distrib/` at `:246` still places the files where `dist` collects them).
- **Backfill-style workflow:** none for OCX packages. The only dispatch-with-input re-publish is `docker-publish.yml` `workflow_dispatch.tag` (container images). The backfill/maintenance workflow is "designed, not built" (`system_design_publisher_trust.md:397-404`, slice 2: dispatched `release.yml` from `main`, `sign` job with repo guard and self-verify, reviewer-gated maintenance workflow for backfills and corrected SBOMs).
- **Keyless identity a signature would carry** (the Fulcio SAN is the `job_workflow_ref`, i.e. the *called* workflow; `sdkgen-publish.yml:15-19` and `packaging/sdkgen/trust-policy.toml:3-14` rely on this) (inferred for the three not yet signing, read for sdkgen):

| Job that would sign | SAN (workflow path @ ref kind) | Issuer |
|---|---|---|
| `sdkgen-publish.yml` `sign` (exists) | `https://github.com/ocx-sh/ocx/.github/workflows/sdkgen-publish.yml@refs/tags/vX.Y.Z` (policy regexp `trust-policy.toml:13`) | `https://token.actions.githubusercontent.com` |
| `oci-publish.yml` `publish`, via `post-release-oci-publish.yml` from `release.yml` | `.../workflows/oci-publish.yml@refs/tags/vX.Y.Z` (inferred: the innermost reusable workflow owning the job; the ref is the release tag ref the caller ran under) | same |
| `oci-publish.yml` `publish`, via `deploy-dev.yml` | `.../workflows/oci-publish.yml@refs/heads/<dispatch ref>` (a branch ref; inferred) | same |
| a new top-level dispatched workflow (not existing) | its own path @ `refs/heads/main` | same |

  Direct (non-reusable) example that exists: `e2e-package` signs as `https://github.com/ocx-sh/e2e-package/.github/workflows/release.yml@refs/heads/main` (`scripts/publish.sh` builds it from `GITHUB_WORKFLOW_REF`) and `release-tag.yml@refs/tags/v*`.
  Docs say the same: for a reusable workflow `build_signer_uri` names the *called* workflow, so `build_signer_digest` pins its commit (`website/.../guides/pin-signer-identity.mdx`, "Claims" section).

### 1.2 CLI signing surface (Q2, Q5)

Source: `crates/ocx_cli/src/command/package_{sign,attest,sbom,verify,push}.rs`.

| Command | Acts on | Notable flags |
|---|---|---|
| `package sign <id>` | **Whatever the reference resolves to.** `--platform/-p` is optional and narrows into one platform of an index; omitted on an index = the index is the subject (`package_sign.rs:27-42`). Given against a single manifest it fails. Sweeps `--tags/--tags-file` refuse `--platform` (`:37-42`) | `--key`, `--fulcio-url`, `--rekor-url`, `--identity-token-file`/`-stdin` (no raw token flag, `:7`), `--no-tty`, `--signature-format`, `--rekor-upload`, `--no-cache` |
| `package attest <id>` | same rule (`package_attest.rs:27-45`) | `--predicate PATH` (<= 15 MiB), `--type` (cyclonedx, spdx, spdxjson, slsaprovenance1, link, vuln, openvex, custom, or URI), signing flags |
| `package sbom <id>` | read side: list/extract/summarize SBOM attestations; same identity resolution as verify (`package_sbom.rs:1-5`); `-p` optional (`:28-37`); `--output`, `--summary` (CycloneDX 1.5-1.7), `--type`, `--no-verify`-style permissive mode | |
| `package verify <id>` | `-p` optional (`package_verify.rs:27-33`); `--certificate-identity/--certificate-oidc-issuer` optional "exactly when the index root lists `publishers` or a `[trust.policy]` matches" (`:36-60`); `--attestation [--type]` (`:87-98`); `--allow-unlogged-signature`; `--sigstore-trusted-root` | |
| `package push --sign / --sbom PATH` | `--sign` signs **each platform manifest the push wrote**, "never the image index, whose digest is rewritten every time another platform merges"; index to be signed afterwards by `sign --tags-file` (`package_push.rs:159-173`). `--sbom` = `attest --type cyclonedx` on the pushed platform digest, CycloneDX only (`:159-160`, `attest_sbom` `:562-600`, platform passed `Some(platform)`) | signing modifiers refused without `--sign`/`--sbom` (`:32-38`) |

- `--platform` defaulting: `install`-family `--platform` "Defaults to the host platform" (`options/platform.rs:16-18`). `sign`, `attest`, `sbom`, `verify` have **no default**: absence means the index itself (`package_sign.rs:27-32`). Install-time verification targets the platform-selected manifest ([ocx-sh/ocx#694](https://github.com/ocx-sh/ocx/issues/694) body).
- SBOM storage (`ocx_sign/src/attest/pipeline.rs`, `ocx_oci/src/referrer/media_types.rs`):
  - **Signed** (a signing identity is visible or `--key` given; decided in `ocx_package_manager/src/tasks/attest.rs:94-99`): an OCI referrer whose `artifactType` and layer type are `application/vnd.dev.sigstore.bundle.v0.3+json` (`media_types.rs:9`; `attest/pipeline.rs:242-243`), carrying a Sigstore bundle whose content is a **DSSE envelope** (`application/vnd.in-toto+json` payload, `attest.rs:23-24`) of an in-toto Statement with the SBOM as predicate; annotations include the predicate type (`pipeline.rs:238`). Optional cosign-style `.att` sidecar under `--signature-format`.
  - **Unsigned** (no signing material visible): the raw SBOM bytes as referrer, `artifactType` = the SBOM media type (`application/vnd.cyclonedx+json`, `application/spdx+json`, `text/spdx`; `media_types.rs:12-18`), no annotations (`pipeline.rs:296-330`). `.sbom` sidecar tag is not written (`pipeline.rs:6`).
  - **"A signature of an SBOM" is the attestation's own DSSE signature**, one referrer, not a second referrer. No code path writes a separate signature referrer anchored on an SBOM referrer (none found). The copy code nevertheless budgets for one: "a signature over an SBOM is depth 2" (`ocx_oci/src/copy.rs:23`).
- Image signature itself: sigstore bundle v0.3 referrer containing a DSSE Statement with predicate type `https://sigstore.dev/cosign/sign/v1` (`attest.rs:50-52`, `sign/pipeline.rs:358`, `:393-394`); `messageSignature` is gone (`sign/bundle.rs:590-614`).
- **What #694 says is impossible on ghcr.io** ([ocx-sh/ocx#694](https://github.com/ocx-sh/ocx/issues/694), OPEN): sign, attest or verify of an **image-index subject**; only platform manifests (`-p`) work. Table: `push --sign --sbom`, `sign/attest/verify -p` work; `sign <id>`, `attest <id>`, `verify <id>` on an index fail (exit 79 for sign). Cause: `SIGNABLE_MANIFEST_TYPES` omits the index media type (`ocx_oci/src/media_type.rs:25`), used as `Accept` for the subject fetch (`sign/pipeline.rs:255`, attest `:187`, verify `:2139`); ghcr.io answers 404 when Accept lacks the index type; `registry:2` and zot ignore Accept, so the acceptance suite never saw it. Second defect: subject descriptor hard-codes `OCI_IMAGE_MEDIA_TYPE`. Still unfixed at origin/main f5e6e66 (`media_type.rs` list read, index type absent). The issue ends: "Release signing of ocx itself on ghcr.io will hit this as soon as it signs an index."
  - Contradiction to flag: `sdkgen-publish.yml:250-257` already signs the **index** on ghcr.io (`ocx package sign ...@<index_digest>`), the exact case #694 reports as failing. No release has run that job on a build with the bug confirmed in this recon (not checked).

### 1.3 Copy and mirror paths in the client (Q3)

- `ocx package copy` (`package_copy.rs`, `ocx_package/src/publisher/copy.rs`): "Leaf manifests and blobs are copied verbatim, a tag's index is merged per platform, and rolling tags are recomputed against the target" (`copy.rs:6-7`). `--platform` repeatable: against a tag it filters the source index; omitted = all platforms (`package_copy.rs:30-37`).
  - **Digest preserved for leaves**: "copied verbatim, never rebuilt: a re-serialised manifest orphans every signature" (`ocx_oci/src/copy.rs:6-7`); `push_manifest_raw(leaf_bytes)` at `:166`.
  - **Referrers**: default-on `--referrers` / `--no-referrers` (`options/referrers.rs`); per **leaf** (`copy_leaf(..., request.referrers, ...)`, `copy.rs:260-266`), following referrer chains to `MAX_REFERRER_DEPTH = 8`, `MAX_REFERRERS_PER_LEAF = 256` (`ocx_oci/src/copy.rs:23-27`). Requires the OCI 1.1 Referrers API at both ends; a target without it fails (`copy.rs:177-182`; `referrers.rs` help text). Cosign `.sig/.att/.sbom` sidecar tags are copied verbatim under the same tag, whether or not the target serves Referrers (`ocx_oci/src/copy.rs:170-175`, `:230-243`).
  - **Index is not byte-identical, and index-level referrers are not carried** (inferred from structure: the copy loops over leaves and `merge_platform_into_index` rebuilds the target's index, `copy.rs:286-318`; referrers are only fetched in `copy_leaf`). A source index named by digest is refused (`IndexNamedByDigest`, `copy.rs:36-44`).
  - **Partial platform copy**: only requested platforms are written; a platform the target has and the source does not is kept and reported `KeptNotInSource` (`copy.rs:139-141`, `:275-283`) so the target index becomes a mixed set; its digest differs from the source index digest.
- `ocx index sync <registry...>` (`index_sync.rs:1-30`): refreshes the *local* index (tag to digest pins) from the registry's catalog; it copies no registry content and no referrers.
- `[mirrors."<host>"]` (`ocx_config/src/mirror.rs:1-8`): per-host rewrite, roles disjoint: `registry` rewrites only `/v2` traffic, `index` only `/config.json`, `/c`, `/p`; "identifiers, digests and on-disk paths stay keyed to the upstream host". A read-time redirect, not a copy. Forwarded via `OCX_MIRRORS`.

### 1.4 Index signing (Q4)

- **Not present**: no `ocx index sign` command (`crates/ocx_cli/src/command/index*.rs` has `catalog, common, list, regenerate, sync, update`); no `expires` handling; no index signer list. They are "designed, later" in `system_design_publisher_trust.md:403` ("Slice 4, catalog signature: ... `expires` (30 d, 7-day re-sign...), `index_expiry = "ignore"`, package-file hash check, `ocx index sign`, `[sigstore."<host>"]` tables") and `adr_publisher_trust_distribution.md:54` (option B "index signs its catalog keylessly") and `:185` ("Catalog signature and Sigstore instances (designed, later)").
- **Present: per-package `publishers`** (merged 2e3750857 "verify packages against publishers declared in the index", ancestor of origin/main): root array of `keyless` (issuer + `identity`/`identity_regexp`/`claims`) and `key` (`key_pem`) entries; classification Invalid/Unrecognised/Usable; refusals `publishers_unrecognized` (exit 82), `publishers_invalid` (exit 65), `publishers_required` (`website/.../guides/pin-signer-identity.mdx` "Declare publishers in the index"; `explanation/public-index.md:121-123`; `crates/ocx_schema/tests/golden/errors.json:2062-2073`). Effective list is merged committed vs fetched: "an index can add a check or narrow one, never widen or delete it" (`ocx_index/src/chained_index.rs:17-41`). Per-registry `signatures` setting `if_declared`/`required`/`off` (`ocx_schema/tests/golden/config.json:174-195`). The `sigstore` publisher field is "built in for ocx.sh ... becoming usable" in slice 4 and is currently treated as unrecognised (e2e-index `e2e/future` fixture).
- **Present, partial: client-side hash cross-check of a root against `c/index.json`.** `IndexStore::read_root` derives `root_catalog_entry(bytes)` = `sha256:<hex>` of the root and compares it to the catalog entry (`ocx_index/src/store.rs:259-300`); on disagreement it *recovers* the catalog entry ("root/catalog straddle, benign", `:299-321`) rather than refusing; applied only for published sources (`local_index.rs:521-543`). Dispatch objects (image-index CAS files) are hash-verified on read and write (`store.rs:93-156`). This is integrity of local state, not a signature or an authenticity gate (inferred from the recovery behavior).
- Goal file for the first slice: `.agents/goals/release-signing-trust-default-2026-11-12.md` (in origin/main) scopes it "to the first slice only" and lists production index, real releases of ocx/ocx-mirror/grimoire and indexbot as out of scope.

### 1.5 ocx's own SBOM (Q6)

- Release: `cargo cyclonedx -v` in the cargo-dist `build-global-artifacts` job (`release.yml:214-257`, cargo-cyclonedx 0.5.5 installer, `:217`); `.cdx.xml` per workspace crate to the GitHub Release; one job, so **not per platform** (the target-triple is not passed; inferred that content is platform-agnostic).
- Docs: `ocx.toml:35` `[group.docs.tools] cargo-cyclonedx = ocx.sh/cyclonedx/cargo-cyclonedx:0.5.9`; `website/sbom.taskfile.yml:1-30` runs `cargo cyclonedx --format json --manifest-path crates/ocx_cli/Cargo.toml` and `scripts/sbom-to-markdown.py` writes `reference/dependencies.md`. Different cargo-cyclonedx versions (0.5.5 in release vs 0.5.9 for docs); the docs task itself installs with `cargo install --locked` (`:7-10`), not the group pin.
- No SBOM is attached to any OCX package of ocx itself.

---

## 2. ocx-mirror (050aab5), ocx-mirror-sdk (98f1cf3), mirror-pypi (c8f5cf8)

### 2.1 ocx-mirror end to end (Q7)

- CLI (`src/command.rs`): `package` (`sync`, `check`, `validate`, `pipeline ...` incl. `generate`, `plan`, `prepare`, `push`, `announce`, `cascade`, `describe`, `notify`, `patch`, **`sign`**), `registry sync`, `dist sync`, `schema`, `version`.
- Sources (`crates/ocx_mirror_spec/src/source.rs:24-61`, `ocx_mirror_source/src/`): `github_release`, `url_index` (remote / inline / generator), `pylock`, `pypi`. Plus a `cargo` source kind appears in artifacts (`system_design_cargo_source_kind.md`), not verified in code.
- Publishing uses **both**: in-process `ocx_package::publisher::Publisher` for github/url legs and `ocx` subprocess (`ocx package create/push/announce/sign`) for pypi, `pipeline push`, `patch` (`crates/ocx_mirror_pipeline/src/ocx_cli/{push,create,announce,sign}.rs`; discussion `mirror-signing.md` threads "two push legs").
- **Registry-to-registry mirroring exists**: `registry sync` copies whole OCX index sources into a destination registry and writes a servable index tree (`registry_sync.rs`, `registry_copy.rs:1-16`: "A mirror is a transport and filters nothing": every tag key and every `manifests[]` descriptor travels by digest, no cascade, no platform predicate). It carries **referrers** (depth ceiling 2, count ceiling 64, `registry_copy.rs:136-161`) and **cosign sidecar tags** `.sig/.att/.sbom` verbatim (`:163-192`), and writes the OCI fallback tag index on destinations without the Referrers API (`ReferrerDestination`, `:477-483`). This supersedes v1 "detect referrers, refuse to copy" in `adr_registry_mirror_sync.md` (per `adr_mirror_signing.md` header).
- **Signing exists**: `sign:` spec block (`ocx_mirror_spec/src/sign_config.rs`: `Ref :53`, `KeylessConfig :118`, `KeyConfig :157`, `KeyFullConfig :198`, `SignConfig :225`); `ocx package push --sign` on subprocess legs plus an index sweep afterwards, and in-process `invoke_sign_reference` for the Publisher leg (`ocx_cli/sign.rs:1-17`); endpoints always passed as explicit `--fulcio-url/--rekor-url` defaulting to public Sigstore (`:33-45`); secrets via child env, never argv (`:19-22`); `--remote` tag resolution (`:47-62`). Backfill: `ocx-mirror package pipeline sign` signs what the target already holds, filters to "not signed by this signer", `--identity/--issuer`, `--force`, `--dry-run`, operator verb, rendered by no generated workflow (`src/command/package/pipeline/sign.rs:1-40`; module `crate::pipeline::sign_backfill`). The design record is `adr_mirror_signing.md` (header says Status Proposed; the code above exists, so the status line is stale, inferred) and the ratified discussion `.agents/discussions/mirror-signing.md` (trust claim: mirror signs what it *produces*; registry-to-registry copies carry upstream signatures verbatim and add none, lines 66-72).
- **SBOM handling: none.** No `sbom/cyclonedx/attest` code; only `.att/.sbom` sidecar-suffix constants (`registry_copy.rs:192`) and comments. The discussion says "SBOM: no, only the signatures" and lists "SBOM generation by the mirror" out of scope (`mirror-signing.md:23-24`). Referrers of any artifact type are copied, including SBOMs a source attached (decision "Referrer copy scope", `:88-92`).
- ocx-mirror's own release (`.github/workflows/release.yml`): tag `v*`, verify + readiness gate, 6-target build via `build-matrix.yml`, `publish-registry` (`:93-120`) calls `oci-publish.yml` for `ghcr.io/ocx-sh/ocx/mirror` (logical `ocx.sh/ocx/mirror`), `announce: true`; GitHub Release with bare archives for four targets. **No `id-token`, no sign, no SBOM** in `release.yml`/`oci-publish.yml` (grep over `.github/workflows` finds `id-token` only in `claude.yml` and `docs.yml`). Its `oci-publish.yml` is a near copy of ocx's (pushes per platform, `--cascade`, then description push and announce).
- Rendered mirror workflows do grant `id-token: write` when `sign:` is set (e2e-mirror `mirror-signing.yml:383-388`); that rendering path is `generate/ci/permissions.rs` in ocx-mirror (not opened).

### 2.2 SDK and what the PyPI flow knows (Q8)

- **The SDK has no plugin interface.** `ocx-mirror-sdk` is a Python library for authoring *generator scripts* that emit a `url_index` JSON document: `IndexBuilder`, `github`/`gitlab` release listing, `http`, `cache`, `text`, `errors` (`src/ocx_mirror_sdk/*`, 818 lines in total; README "Python SDK for authoring ocx-mirror generator scripts"). A generator is run by ocx-mirror as a subprocess (`ocx_mirror_source/src/generator.rs`). Nothing is "implemented" by a plugin.
- Per-release metadata returned by a generator: version -> `{prerelease: bool, assets: {filename: url}}` only (`schemas/url-index.v1.json`, `docs/schema/url-index.md`). **The SDK schema is behind the mirror**: ocx-mirror also accepts an asset object `{url, sha256}` (`ocx_mirror_source/src/url_index.rs:53-82`, `IndexAsset::Verified`, "stays url-index v1"), the SDK's schema file allows string assets only.
- ocx-mirror's in-memory per-version record: `VersionInfo { version, assets: name->Url, asset_digests: name->sha256 (sparse), is_prerelease }` (`ocx_mirror_source/src/lib.rs:25-33`). GitHub supplies `asset.digest` when present (`github_release.rs:308-322`). No licence, dependency, or SBOM field.
- PyPI/pylock flow (`ocx_python`, in the ocx repo, origin/main): per package `name`, `version`, `marker`, wheels each with `filename`, `url`, **`sha256`** (`crates/ocx_python/src/lock.rs:34-52`; a wheel without a hash is a `LockError::MissingHash`). At repack time: every installed path from the wheel `RECORD` (`repack.rs:39-40`) and a `WheelDescription { summary, keywords, license }` from `*.dist-info/METADATA` (`repack.rs:151-160`; only `Summary`, `Keywords`, `License` headers are read, `parse_wheel_metadata` `:190-`). **`Requires-Dist` is not read**; dependency resolution is delegated to `uv` (committed `pylock.toml`; `ocx_mirror_source/src/pypi.rs:52-53` "hashes, metadata pointers and requires-python belong to resolution, which is uv's job"). The composed OCX package has one dependency: the pinned interpreter package (`compose.rs:62-63`, `:199`). The `mirror-pypi` specs name `python.interpreter_package` per mirror (`black/mirror.yml`).

### 2.3 mirror-pypi (c8f5cf8)

- Five mirror specs (`pycowsay`, `black`, `streamlit`, `yt-dlp`, `google-cloud-aiplatform`), each with generated `mirror-*.yml`, `describe-*.yml`, `patch-*.yml` workflows plus `verify-generated.yml` and `bootstrap.yml`. Target `dev.ocx.sh/ocx/<name>`; pushes only when `OCX_MIRROR_REGISTRY_TOKEN` is configured (`mirror-black.yml:385-407`). README: "WIP". **No `sign:` block, no `id-token` in the workflows** (grep for `sign|id-token|sbom` in `mirror-black.yml` returns no signing hit).

---

## 3. e2e-mirror and ocx-mirror-e2e (Q9)

- `e2e-mirror` = `ocx-contrib/e2e-mirror` (remote `git@github.com:ocx-contrib/e2e-mirror.git`), a4c9fc6. "The tier-2 canary for ocx-mirror" for publish-side signing: `signing/mirror.yml` mirrors `pycowsay` from PyPI into `dev.ocx.sh/ocx/e2e-signing` with `sign: keyless: {fulcio, rekor}` (public endpoints, explicit) and cascade; generated workflows `mirror-signing.yml` (`workflow_dispatch`, `id-token: write` at `:383-388`), `describe-`, `patch-`, `cascade-signing.yml`, `verify-generated.yml`. It exists to prove four things offline harnesses cannot (README): real Fulcio exchange, `id-token: write` granted by rendered workflows, `ocx package verify` against the workflow identity, **and the index above the platform manifests carrying a signature** ("that leg broke twice during development"). It pins a `dev.ocx.sh` ocx-mirror build. Everything goes to `dev.ocx.sh`; nothing to GHCR.
- **`ocx-sh/e2e-mirror` does not exist**: `gh repo view ocx-sh/e2e-mirror` -> "Could not resolve to a Repository". `gh repo view ocx-contrib/e2e-mirror` -> PUBLIC, pushedAt 2026-09-03T18:41:54Z.
- `ocx-mirror-e2e` = remote `https://github.com/michael-herwig/ocx-mirror-cmake.git`, dc34549 "the servable index tree produced by the first full mirror". README: an end-to-end exercise of `ocx-mirror registry sync`: source index `https://index.ocx.sh`, source content `oci://ghcr.io/ocx-contrib/kitware/cmake`, destination `ghcr.io/michael-herwig/ocx-mirror-cmake/ocx.sh/kitware/cmake`; tracked content is the served tree `public/ocx.sh/{config.json,c/index.json,p/kitware/cmake.json,p/kitware/cmake/o/sha256/*.json}` (dozens of image-index CAS files). **No workflows, no signing.** `gh repo view michael-herwig/ocx-mirror-cmake` -> "Could not resolve" (private or not on that name; not determined). One untracked file `issue-uncapped-reads.md` in the working tree (not read).

---

## 4. ocx-indexbot (c7434553, tag v0.6.3; origin/main a79f7f3), index (f8c3b28f), index-template

### 4.1 What the bot checks (Q10)

- Commands: `announce`, `reconcile`, `validate`, `validate-pr`, `classify-pr`, `governance-check/-gate/-poll`, `render`, `stale`, `seed-import`, `ci`, `workflows-check`, `schema` (`src/ocx_indexbot/cli/`). Reference deployment `ocx-sh/index` (README).
- Ingest checks (`core/validate_entry.py:1-30`): path<->name derivation, repository host allowlist (SSRF ordering), reserved namespaces and tags, digest-hex `fullmatch`, CAS content-digest self-consistency, dangling references; registry truth checks in `core/registry_checks.py`/`observe.py` (re-observes each tag's index from the physical registry).
- **Compare old and new roots: yes.** `core/diff.py:97-145` `classify_change(before, after)` -> `new-package | refresh | human-review-required`. "Machine lane is narrow by design": any change to `repository, owners, status, deprecated_message, created, upstream, superseded_by`, or a changed `yanked`/`ephemeral` on an existing tag, goes to human review (`:117-145`).
- **Signatures: none verified.** Grep for `signature|cosign|sigstore` in `src/` and `docs/reference|guide` finds nothing relevant; the bot never fetches referrers.
- **`publishers` is not known to the bot, at v0.6.3 or origin/main.** `PackageRoot` has no such field (`model.py:226-275`); `parse_package_root` reads named keys only and ignores others (`validate_entry.py:673-722`); `serialize_package_root` emits only the known keys (`:642-670`); `validate` byte-compares the committed root against `serialize_package_root(parse_package_root(raw))` and fails "committed bytes are not the canonical root serialization (byte-exact discipline)" (`cli/validate.py:225-238`). Consequences read directly from that code: (a) a root carrying `publishers` fails the bot's `validate` byte gate; (b) any bot-written root (announce/reconcile/regenerate) would drop the key; (c) `classify_change` has no `publishers` term, so a root edit that touched only `publishers` would not reach `human-review-required` (inferred: it follows from the field list in `diff.py:117-145`).
- Deployment and tests: released to PyPI `ocx-indexbot` via trusted publishing (`release.yml:60-77`, environment `pypi`); consumed pinned exactly (`ocx-indexbot==0.6.3`) in `index/bot-tools/pyproject.toml:23` "so the version running in the privileged governance job changes only through a reviewed PR". CI: `ci.yml`, `audit.yml`, `mutmut.yml`, `docs.yml`; the index repo adds deployment-specific `bot-tools/tests/security/`.
- e2e-index uses of the bot: **none** (confirmed): the only `indexbot` hits in the worktree are inside `node_modules/@ocx-sh/{theme,catalog}`. Its roots are hand-built by `scripts/build_index.py` (reads each tag's image index from ghcr.io anonymously and writes `docs/`), rendered by `bun x ocx-catalog build`, and the Pages job diffs `docs/` against the rendered output (`pages.yml:28-30`).

### 4.2 index repo schemas and deploy (Q11)

- `schema/root.schema.json`: root `required` = `name, repository, owners, status, deprecated_message, created, desc, tags`; **`additionalProperties: false` at the root** (`:99`) and no `publishers` property (same on origin/main 6c67672b, string absent). So a root with `publishers` fails schema validation too. `c-index.schema.json` has `additionalProperties: false` (`:26`) and no `expires`/signature property. Other schemas: `config.schema.json`, `image-index.schema.json`.
- Workflows: `validate.yml` (`indexbot validate-pr`), `governance.yml`, `reconcile.yml`, `stale.yml`, `pr-checks-label.yml`, `ci.yml`, `catalog-ci.yml`, `dependency-review.yml`, **`render-deploy.yml`** (deploy job).
- Deploy (local, stale checkout): `render-deploy.yml` on `push: main` + `workflow_dispatch` (also fired by governance `deploy-on-merge`); `task render:build` = `ocx-catalog build` then `indexbot render --out` into the same tree (order is fixed, `:117-125`), `schema:validate:rendered`, then **Cloudflare Pages** `wrangler pages deploy ... --project-name=ocx-index`, domain attach for `index.ocx.rs` / `index.ocx.sh`. **origin/main (329 commits ahead of this checkout) differs**: adds a catalog HTML deploy to **Bunny** storage at `https://ocx.sh/catalog/` and a `deploy-index-bunny` job for the index wire files (`scripts/bunny-upload.mjs dist`, `BUNNY_EDGE_STORAGE_KEY`), with Cloudflare Pages kept as DNS rollback (diff of `render-deploy.yml` HEAD..origin/main; I did not read the whole file).
- `index` depends on `@ocx-sh/catalog ^0.5.0` (`package.json`); the catalog repo is at 0.6.3.

### 4.3 index-template

- `grimoire-rs/index-template` is the **Grimoire** (`grim`) package index template, not an OCX index: README "AI-config packages — skills, rules, agents, MCP servers, bundles"; `package.json` depends on `@grimoire-rs/indexer` (bumped to 0.6.0 at HEAD); tracked files are only `README.md`, `package.json`, `package-lock.json`, `.gitignore`. No indexbot, no `root.schema.json`, no signing or `publishers` reference (grep of schema returned no file). It generates the `index/` tree, CI and "the trust policy" on `npm run setup`.

---

## 5. ocx-catalog (d560f56, v0.6.3) (Q12)

- Role: "a **renderer**: ... never writes to an index and never invents index data" (`README.md`). Package `@ocx-sh/catalog`, CLI `ocx-catalog build|dev|ci`.
- **Data flow: index files only.** Sources are `path`, `url`, `git` (`sources/path.ts`, `walker.ts`, `git.ts`; `build/sources_pipeline.ts:1-60`). A repo-wide grep for `/v2/`, `ghcr.io`, `manifests/` in `src/` finds no registry client (grep output empty; excluding comment lines). Wire files are mirrored verbatim (`sources/mirror.ts`; e2e-index's Pages job proves bytes unchanged with `diff -r`).
- **Network step and cache**: yes for `url` sources (HTTP walker with ETag/conditional GET and a content-addressed blob cache under `node_modules/.cache/ocx-catalog` or `.ocx-catalog`, deliberately outside the self-sweeping scratch roots, `build/cache_dir.ts:1-40`) and `git` sources (`git clone --depth 1`, execFile, `sources/git.ts`). `path` sources need none.
- **Compatibility ("support") matrix, the details page**: data comes from each tag's `content` digest -> the committed image-index CAS file `p/<ns>/<pkg>/o/sha256/<hex>.json`: `releaseFacts()` parses it and `imageIndexFacts()` extracts per-index `platforms`, `license` (`org.opencontainers.image.licenses`), `source`, `revision` (`site/model/detail.ts:164-176`; `viewmodel/types.ts:244-252`, `viewmodel/catalog.ts:504-506`). `detailWire` dedups identical fact sets into `platformSets` and maps tags to sets (`detail.ts:183-211`, `:258-261`). Rows are written at build as `data/catalog/support/<route key>.json` by `viewmodel/support_matrix.ts:1-50` (tag, platforms, `ephemeral`, `yanked`, package `deprecated`), fetched on first dialog open by `site/client/support_matrix.ts:1-50`. Components: `PlatformMatrix.astro` (rail "Platforms" card: OS glyph + arch cells per distinct platform set; support-matrix `Dialog` with platform filter chips `Tag variant=filter`), `OsGlyph.astro`, `VersionList.astro`/`MajorGroup.astro`/`MinorLine.astro`/`TagChip.astro` for the version tree, `DetailBanners.astro`, `MetaRail.astro`.
- **Per-tag model**: wire root `tags: {tag -> {content: digest, observed, yanked?: {reason, at}, ephemeral?: true}}` (`detail.ts:183-211`; index `root.schema.json` tagEntry). Marks surfaced: yanked (with reason), ephemeral (`EPHEMERAL_REASON`), package-level deprecated + message, `superseded_by`.
- **List cards**: `PackageCard.astro:37-48`: `Tag variant="stamp"` for `deprecated` (tone warning) and `yanked` (tone danger) from `card.status`; keyword chips (`tone="keyword"`); OS glyphs (`OsGlyph`) from `card.platforms` via `displayOses`; tag count; install-command copy button. `PackageRow.astro` is the list-view variant.
- **Provenance in the catalog: explicitly deferred.** `viewmodel/types.ts:236-241`: "Deliberately no provenance/attestation boolean here (ADR Decision 4: signature-verification provenance stays deferred)". `publishers` appears nowhere in `src/` (grep). Shape validation is "NOT this layer's job" (`sources/types.ts:330-345`), so an unknown root key such as `publishers` would be read past, not rejected (inferred; the e2e-index site renders roots carrying `publishers` today).
- Catalog deploy: its own `pages.yml` deploys docs to `https://ocx.sh/apps/catalog/` on main pushes through ocx-sh/website's deploy action; `release.yml` publishes to npm with `--provenance` via OIDC (`:7-26`).

---

## 6. e2e fixtures (supporting facts for the signing story)

- `e2e-package` (`ghcr.io/ocx-sh/e2e-package`): six tags, each signed differently (`main` keyless + 1 SBOM, `two-sboms` keyless + 2 SBOMs about 180 s apart, `tag` signed under `release-tag.yml@refs/tags/v*`, `other` other-signer workflow, `key` cosign key pair, `unsigned`). Single platform `linux/amd64+libc.glibc`. "The signature covers the platform manifest ... the image index is not signed" (README); `scripts/publish.sh` comment: "ocx 0.6.6 cannot sign or verify an image-index subject on ghcr.io". Self-verify: `package verify -p ... --certificate-identity "${GITHUB_SERVER_URL}/${GITHUB_WORKFLOW_REF}"` plus `verify --attestation --type cyclonedx`. Pins `ocx-sh/setup-ocx@25fa771...` `version: '0.6.6'`. Workflow guard `github.repository_id == '1455949009'`.
- `e2e-index` (`e2.ocx.sh`): five roots (`e2e/claims`, `oid`, `tagged`, `none`, `future`) all pointing at `oci://ghcr.io/ocx-sh/e2e-package`; `client-e2e.yml` (manual, builds ocx at a 40-hex ref, runs `test/e2e -m e2e_public`, no secrets). Per goal file the Phase run succeeded (17 passed) at run 43365086954.

---

## negative:

- No `ocx index sign`, no `expires`, no catalog signature, no index signer list in ocx origin/main (only in design docs, slice 4).
- No signing and no SBOM in `ocx/cli` or `ocx/mirror` OCX-package publish paths; `oci-publish.yml` (both repos) cannot mint an OIDC token as written.
- No OCX-package backfill/maintenance workflow exists in ocx; the only dispatchable re-publish is `docker-publish.yml` for container images.
- No SBOM is attached to any ocx-published OCX package; no per-platform SBOM generation anywhere (release SBOM is per workspace crate, one job).
- ocx-mirror: no SBOM generation or attachment; no signing in its own release workflow or in `mirror-pypi` specs.
- ocx-mirror-sdk has **no plugin interface** (assumption in Q8 does not hold), and its url-index schema lacks the `{url, sha256}` asset form that ocx-mirror accepts.
- `Requires-Dist` and per-wheel licence-by-file are not read by the mirror's python flow; only `Summary`, `Keywords`, `License` and `RECORD` paths.
- `ocx-sh/e2e-mirror` does not exist (the repo is `ocx-contrib/e2e-mirror`); `michael-herwig/ocx-mirror-cmake` did not resolve through `gh`.
- indexbot has no `publishers` support (field, parse, serialize, classify) at v0.6.3 or local origin/main; the index root schema forbids unknown root properties, so `publishers` currently cannot be committed to `ocx-sh/index`.
- `index-template` is not an OCX artifact (Grimoire); it shares nothing with indexbot or `root.schema.json`.
- A separate "signature of an SBOM" referrer is not written anywhere in ocx; the SBOM attestation's DSSE envelope is the signature. (The copy code's "depth 2" comment anticipates one.)
- e2e-index does not use indexbot (confirmed).
- Contradiction to an assumption in Q2/Q3: ocx `package copy` **does** carry referrers and sidecars by default (`--referrers` default-on), but only per leaf platform manifest; the index is re-merged, not copied byte-identical.
- Contradiction inside ocx: `sdkgen-publish.yml` signs an **index** on ghcr.io while [#694](https://github.com/ocx-sh/ocx/issues/694) says that fails; unresolved here (not run).
- The local `index` checkout is 329 commits behind origin/main and differs in `render-deploy.yml`; findings about its deploy are from the stale tree plus a partial diff.

## leads:

- `ocx_package_manager/src/tasks/sign.rs` and `attest.rs`: how `sign --tags-file` picks and signs the index digest, and whether it dedups (ocx-mirror discussion says sweeps are non-idempotent and verify caps candidates at 8, `verify/pipeline.rs:91`, not re-checked on origin/main).
- `ocx-mirror/.claude/artifacts/research_signature_backfill_identity.md` and `adr_mirror_signing.md` D4 (backfill pre-filter rule) are the nearest existing design for a "sign what is unsigned" job.
- `system_design_publisher_trust.md` §8 slices 2-4 and `adr_publisher_trust_distribution.md` (rollout order, `sigstore` publisher field, `index_publishers`).
- `ocx_sign/src/verify/pipeline.rs` candidate caps (`MAX_ATTESTATION_CANDIDATES = 32`, total bytes 64 MiB, `attest.rs:13-21`) bound how many SBOM referrers a verifier will consider when backfills accumulate them.
- `crates/ocx_cli/src/api/data/{sbom,attestation,verify}.rs` report shapes and `signatures[]` row: the machine-facing form a catalog or bot would consume (not read).
- `ocx_oci/src/referrer/` discovery and the capability cache (`$OCX_HOME/state/referrers/<registry>.json`) for ghcr.io behavior on index subjects.
- ocx-mirror `generate/ci/permissions.rs` and the rendered `mirror-signing.yml` in e2e-mirror for how `id-token` is granted.
- `index/.github/workflows/governance.yml` (human-lane rules for governance-field edits) once `publishers` becomes a root field; `classify_change` currently has no term for it.
- `ocx-catalog` `src/site/model/detail.ts` `imageIndexFacts` and `releaseFacts.ts`: where a per-platform signature or SBOM badge would attach; the catalog sees only index and CAS files, so a signature state needs either a root/catalog field or a build-time registry step that does not exist.
- ocx `website/.../guides/pin-signer-identity.mdx` and `explanation/signing.mdx` for the published verification story.
