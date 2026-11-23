# Research: ocx patches, dist.json and the Windows shim

## Metadata

**Date:** 2026-11-18
**Lane:** codebase recon (`/hex-discuss`, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** ocx worktree `uno` at 5f1f5962; rules_ocx shallow clone at e311b99; `ocx-sh/www-setup` not read
**Expires:** 2027-05-18
**Caveat:** findings only, no recommendation; items marked UNVERIFIED had no primary source read.

No Bash tool was available, so blob sizes are not `stat`-measured. I did not open `patch_sync.rs`, `patch_why.rs`, `launcher/generate.rs`, or the lock schema in `ocx_project`.

### Findings

**A1/A2. What a patch is and how it is found**
- `[patches]` config has `registry`, `path` (default `{registry}/{repository}`), `required` (default true) and a loader-set `system_locked` (`crates/ocx_config/src/patch.rs:14-45`).
- A descriptor is an OCI artifact with `artifactType application/vnd.sh.ocx.patch.v1`. Its single layer is JSON `{version:1, rules:[{match, packages[], required?}]}`. Limits: 256 rules, 64 packages per rule, 512-byte glob (`ocx_package_manager/src/patch/descriptor.rs:23-39,83-137`).
- Lookup is by tag: `__ocx.patch` on the per-package repository, `InternalTag::PATCH_TAG` (`tasks/patch_discovery.rs:42,407`). The global descriptor is `<registry>/global:__ocx.patch` (`patch.rs:25`). `match` is a flat glob over `registry/repo`, `:tag` or `@digest`; `*` spans `/`, `:` and `@` (`descriptor.rs:89-95`).
- A companion is an ordinary package. It contributes env entries, a `binaries` claim, `integrations`, and its own dependencies (`.claude/rules/subsystem-package-manager.md`, `tasks/resolve.rs` row). Its own `entrypoints` launchers are not put on PATH. I found no file overlay onto the base. The overlay is applied at execution time only.
- Docs: `website/src/content/docs/guides/patch-packages.mdx`. The ADR is `adr_infrastructure_patches.md`, referenced in `patch.rs:5` and `ocx_config/src/patch.rs:5`. I did not open the ADR.

**A3. freeze / frozen**
- `ocx patch freeze` takes no arguments. It writes `patches.snapshot.json` beside the project's `ocx.lock`, or into `$OCX_HOME` under `--global` (`command/patch_freeze.rs:13-15,53-66`). It reads record-only roots (`PatchRootScope::Recorded`, `:28-33`).
- Snapshot schema V2 (`patch/snapshot.rs:15-66`):
  - `companions`: `registry/repo:tag` → digest.
  - `descriptors`: `registry/repo` → descriptor manifest digest.
  - Both maps are `BTreeMap`, so output is byte-deterministic (`:71-87`).
- The snapshot file is not itself named by a digest. `read` returns the sha256 of the file bytes, which the execution record carries (`snapshot.rs:100-131`).
- The snapshot is selected only by env `OCX_PATCH_SNAPSHOT` (`subsystem-package-manager.md`, "Two consequences" bullet 3). It is not part of `ocx.lock`.
- Under a snapshot, compose reads descriptors from the CAS by digest with no tag resolve, and companions by pinned digest. A missing pin is `NotFound`, exit 79 (same rule file, "Two consequences").
- `--frozen` is a different thing: the package-tier index-chain mode (`ChainMode::Frozen`). Companion resolution is mode-independent by design (`subsystem-package-manager.md`, "Patch companions pin in patch state").
- Without a snapshot, pins live in `state/patch-companions/<registry>/<repo>.json` (tag → top manifest digest) and `state/patch-descriptors/…`, advanced only by `patch sync` (`subsystem-file-structure.md`).
- `sync`, `publish`, `why`: I read only the one-line table entries in `subsystem-cli-commands.md`. `why` lists which companion and rule contributes each patched env var.

**A4. Purity of the composition**
- Under a snapshot, the pin inputs are fixed: companion digests and descriptor digests come from the file, the base from the lock. Without one, they are tag-resolved state and can float.
- A companion graft "adds only the vars and claims it has not already emitted" (`subsystem-package-manager.md`, `resolve.rs` row). I found no removal mechanism in what I read.
- No composite digest of (base, descriptors, companions) exists beyond the snapshot file's own sha256.

**A5. Opt-out and the project tier**
- The opt-out is the per-package `no-patches` key in `ocx.toml` (`ocx_project/src/config.rs:98,371-375`). The docs say it applies to `ocx exec`, `ocx env` and `ocx direnv export` (`patch-packages.mdx:121`). It travels to children as a mark in `__OCX_ENV_STATE` (`:123`).
- A SYSTEM-locked required tier overrides it (`patch-packages.mdx:106-119`; `ocx_config/src/patch.rs:60-62`).
- `no-patches` does not change the declaration hash. The test is named `declaration_hash_unchanged_by_no_patches` (`config.rs:1169`).
- I found no companion field in `ocx.lock` in what I read.

**B1. dist.json producer**
- No generator or schema exists in the ocx repo. The only mentions are docs and artifacts. It is served from setup.ocx.sh by `ocx-sh/www-setup` (`.claude/rules/workflow-release.md:77`).
- This repo's `scripts/release_assets.py` writes the archives plus a separate cargo-dist-shaped `dist-manifest.json` (`:181-198`).
- Schema as vendored by rules_ocx: `{"schema":1,"latest":{…},"releases":[{version,channel,tag,target,filename,sha256,url}]}` (`rules_ocx/ocx/private/manifest.bzl:6-9`).

**B2. Consumers**
- In ocx: none in Rust. `ocx self update` and `self setup` use the OCI index. The only other hit is docs (`install-options.mdx:20`).
- `install.sh` and `install.ps1` live in `www-setup`, which I did not read.
- rules_ocx `select_release` (`manifest.bzl:15-36`):
  - It requires `schema == 1`.
  - It returns the first row in list order matching `version` and `target`.
  - It never looks at `filename`.
  - An earlier extra row with the same version and target and a different filename would be picked by mistake.
  - Extra row or top-level fields are ignored. It reads only `schema` and `releases[].version/target`, plus `tag`, `filename`, `sha256` and `url` on the chosen row (`manifest.bzl:83-99`, `download.bzl:43-48`).
- rules_ocx `scripts/bump_ocx.py` also reads `latest.version` and `channel`. It requires at least 8 targets and no dropped target (`:13-14`), and checks `tag` and `filename` for url shape (`:215-241`). It has a duplicate-row guard (`:258-261,284`). I did not check whether it rejects unknown fields.
- `archive_type(filename)` maps `.zip` and `.tar.gz` and defaults to `tar.xz` (`manifest.bzl:49-53`).

**B3. Archive contents**
- Per archive: `ocx` or `ocx.exe` plus `CHANGELOG.md`, `LICENSE`, `LICENSE-THIRD-PARTY.md`, `README.md` (`release_assets.py:47-48,128-130`).
- Tar archives have a top directory `ocx-<triple>/` (`:86,137`). Zip archives are written without a directory entry (`:96-103`).
- Eight targets (`:36-45`): linux gnu and musl on x86_64 and aarch64, apple-darwin on both, and `*-pc-windows-gnullvm` on both. The docstring notes v0.6.6 shipped Windows as msvc (`:14-15`).
- Other release files: per-archive `.sha256`, `sha256.sum`, `ocx-<triple>.cdx.json`, `ocx.cdx.xml`, `dist-manifest.json`.

**C1. Shim crate**
- It has no non-Windows dependencies. Windows adds `windows-sys` and `dunce` (`crates/ocx_shim/Cargo.toml:14-21`).
- It is std-based, not `no_std`.
- `core.rs` (476 lines) holds the pure sidecar parsing, command-line building and quoting. `main.rs::run` (`:136-538`) is the Win32 half (about 400 lines): `CreateProcessW`, a job object, a handle list, a ctrl handler.
- The three sidecars, probed in this order (`core.rs:117`):

| Sidecar | Value | Wire verb |
|---|---|---|
| `<stem>.shim` | absolute package root | `launcher exec` |
| `<stem>.shimref` | pinned identifier | `launcher shim` |
| `<stem>.exec` | home, plus optional baked absolute `ocx` | `[--project <root> \| --global] exec` |

- Verbs are at `core.rs:370-386`. All are `… -- <stem> <argv…>`. Sidecar rules: one line, at most 32 KiB, no NUL/CR/LF, UTF-8 (`:126-178`).
- The ADR (`adr_windows_exe_shim.md:72,301`) says blobs are about 128–138 KiB each. The code says `SHIM_SIZE_BUDGET = 512 KiB` (`ocx_store/src/shim.rs:14`) while the ADR says 256 KiB. These disagree.

**C2. Build and embed**
- Two committed blobs, `crates/ocx_store/src/shims/ocx-shim-{x86_64,aarch64}.exe`, embedded with `include_bytes!` on Windows targets only (`shim.rs:17-28`). They are empty off Windows (`:30-34`).
- Built with cargo-zigbuild and Zig 0.16.0 via `task rust:shim:build` and `build-windows-shims.yml` (`shim.rs:6-11`). Targets are the two gnullvm Windows triples (`build-windows-shims.yml:113-114`).
- Guards: `SHIM_SHA256` canary, a ban on VERSIONINFO, and a canary that the blob contains the `.exec` literals. The source-change-without-blob-refresh drift is caught only by that last canary (`shim.rs:355-409`).
- One blob is hardlinked under N tool names (`shim.rs:276-278`).

**C3. Direct execution**
- No mode execs a target directly. Every sidecar spawns `ocx` (`main.rs:253-256`).
- Already present: `SetEnvironmentVariableW` is used only to strip `OCX_GLOBAL` and `OCX_PROJECT` (`:238-248`). The child inherits the parent env, `lpEnvironment = NULL` (`:441`). The shim's own directory is known (`module_dir`, `:186`).
- Absent today: a sidecar grammar for env entries or a target path, any path resolved relative to the shim, env-block construction, and a new probe-order entry.
- A new sidecar needs a refreshed blob, new `SHIM_SHA256` values and new canaries. Wire tokens are paired goldens with `body.rs` (`core.rs:370-386`).

**C4. POSIX launchers** (`launcher/body.rs`)
- `.sh` launcher (`:15-22`):
  ```
  #!/bin/sh
  # Generated by ocx at install time. Do not edit.
  exec "${OCX_BINARY_PIN:-ocx}" launcher exec '<pkg_root>' -- "${0##*/}" "$@"
  ```
- Deferred-tool shim (`:29-37`): the same shape, with `launcher shim '<id>'`.
- Trampoline (`:74-100`): `unset OCX_GLOBAL OCX_PROJECT`, then `__ocx_binary='<abs ocx>'`, then `exec "${OCX_BINARY_PIN:-$__ocx_binary}" --project '<root>'|--global exec -- "${0##*/}" "$@"`.
- All POSIX variants re-enter `ocx`. None exec a target directly. I did not read `generate.rs`.

### negative: looked for and not found
- A `dist.json` generator, schema file or test in this repo.
- A Rust consumer of `dist.json`.
- A patch field in `ocx.lock` (lock schema not opened).
- Any shim mode that skips `ocx`.
- Measured blob sizes (no stat available).

### leads
- `www-setup` (`install.sh`, `install.ps1`, dist.json generator) is where B2 stays unverified.
- `ocx_project` lock types, to confirm A5.
- `adr_infrastructure_patches.md`, `adr_windows_exe_shim.md` and `system_design_windows_exe_shim.md`.
- `.claude/artifacts/research_package_bundle_recon.md` and `research_signing_dogfood_dev_channel.md` both mention dist.json, so they may already cover B.
- `rules_ocx/scripts/bump_ocx.py` for strictness on unknown fields.
